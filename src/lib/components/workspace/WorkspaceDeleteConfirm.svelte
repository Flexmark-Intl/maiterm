<script lang="ts">
  /** Delete a workspace from its right-click menu: says what will stop and what goes, then asks
   *  for the Delete button to be HELD for a second, so a stray click or a habitual Enter can't
   *  delete anything. A live workspace is suspended first and then deleted (exactly the two
   *  steps the human used to take by hand); a Hive among what goes is deleted through Hive
   *  delete first, with its gallery and shared memory, so its worktrees are cleaned up rather
   *  than left behind (docs/hive.md §11.2). A Hive delete that refuses (a land in flight) stops
   *  everything before anything is deleted. */
  import type { HiveDeletePreview, Workspace } from '$lib/tauri/types';
  import { hiveDelete, hiveDeletePreview } from '$lib/tauri/commands';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { stackStore } from '$lib/stores/stack.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { isServiceTab, isStripHidden } from '$lib/workspace/hiddenTabs';
  import { deleteRefusal, keptLines } from '$lib/hive/delete';
  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import { untrack } from 'svelte';

  interface Props {
    workspace: Workspace;
    /** Workspaces shown under it (its Hives): deleted with it. */
    children?: Workspace[];
    /** The row's rectangle: the confirmation sits under it (over it, near the bottom). */
    anchor: DOMRect;
    /** Delete one workspace that is already suspended. */
    remove: (workspaceId: string) => Promise<void>;
    /** Close THIS confirmation: names the workspace it was for, so a delete that finishes after
     *  another confirmation has opened can't close that one. */
    onclose: (workspaceId: string) => void;
    /** A delete started or ended: the sidebar opens no other confirmation meanwhile. */
    onbusy?: (busy: boolean) => void;
  }

  let { workspace, children = [], anchor, remove, onclose, onbusy }: Props = $props();
  // The props are read through getters that follow the sidebar's CURRENT target, so nothing
  // that runs after an await may read them: it would act on whichever workspace is named by
  // then. Everything a delete touches is fixed here, once.
  const id = untrack(() => workspace.id);
  const close = () => onclose(id);

  const HOLD_MS = 1000;
  const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

  const going = $derived([workspace, ...children]);
  const hives = $derived(going.filter((w) => w.hive));

  let previews = $state<Record<string, HiveDeletePreview | string>>({});
  $effect(() => {
    for (const h of hives) {
      const id = h.id;
      hiveDeletePreview(id)
        .then((p) => { previews = { ...previews, [id]: p }; })
        .catch((e) => { previews = { ...previews, [id]: `Couldn't read what ${h.name} holds: ${String(e).replace(/^Error: /, '')}` }; });
    }
  });

  /** One sentence per fact, from the workspace's own state. */
  const lines = $derived.by(() => {
    const out: string[] = [];
    let tabs = 0;
    let archived = 0;
    let services = 0;
    let working = false;
    for (const w of going) {
      tabs += w.panes.flatMap((p) => p.tabs.filter((t) => !isStripHidden(t))).length;
      archived += w.archived_tabs?.length ?? 0;
      for (const s of stackStore.services(w.id)) {
        if (!s.shared_from) services += stackStore.liveCopies(w.id, s.id).length;
      }
      const signal = w.panes.flatMap((p) => p.tabs.filter((t) => !isServiceTab(t)).map((t) => t.id));
      if (claudeStateStore.getWorkspaceClaudeState(signal) === 'active') working = true;
    }
    if (tabs) out.push(tabs === 1 ? 'Its tab closes, and any agent in it ends.' : `Its ${tabs} tabs close, and any agents in them end.`);
    if (services) out.push(services === 1 ? '1 running service stops.' : `${services} running services stop.`);
    if (working) out.push('An agent is working there right now.');
    if (archived) out.push(archived === 1 ? 'Its archived tab goes too.' : `Its ${archived} archived tabs go too.`);
    out.push('Its tasks and notes are deleted with it.');
    if (children.length) out.push(`${children.length === 1 ? 'The workspace under it is' : `The ${children.length} workspaces under it are`} deleted too: ${children.map((c) => c.name).join(', ')}.`);
    for (const h of hives) {
      const p = previews[h.id];
      if (!p || typeof p === 'string') continue;
      const unlanded = p.unlanded.length;
      const kept = keptLines(p, true, true).length;
      out.push(
        `${h.name}'s gallery and shared memory are deleted; its branches are kept${unlanded ? `, ${plural(unlanded, 'branch', 'branches')} with commits not landed` : ''}.`
        + (kept ? ` ${plural(kept, 'folder')} holding work stay on disk.` : ''),
      );
    }
    return out;
  });

  const reading = $derived(hives.some((h) => !(h.id in previews)));
  const refusal = $derived.by(() => {
    for (const h of hives) {
      const p = previews[h.id];
      if (typeof p === 'string') return p;
      const r = deleteRefusal(p ?? null);
      if (p && r) return `${h.name}: ${r}`;
    }
    return null;
  });

  let busy = $state(false);
  let failure = $state<string | null>(null);
  const canDelete = $derived(!reading && !refusal && !busy);

  async function perform() {
    // Fixed before the first await (see `id` above). Children first: the parent's row is the
    // anchor, and it goes last.
    const doomed = [...children, workspace].map((w) => ({ id: w.id, hive: !!w.hive }));
    if (doomed[doomed.length - 1]?.id !== id) return;
    busy = true;
    failure = null;
    onbusy?.(true);
    try {
      for (const d of doomed.filter((d) => d.hive)) {
        const r = await hiveDelete(d.id, true, true);
        workspacesStore.applyHiveDeleted(d.id, r.active_tabs);
      }
      for (const d of doomed) {
        const w = workspacesStore.workspaces.find((x) => x.id === d.id);
        if (!w) continue;
        if (!w.suspended) await workspacesStore.suspendWorkspace(d.id);
        await remove(d.id);
        logInfo(`workspace: deleted ${d.id.slice(0, 8)} from the sidebar`);
      }
      onbusy?.(false);
      close();
    } catch (e) {
      busy = false;
      onbusy?.(false);
      failure = `Nothing more was deleted: ${String(e).replace(/^Error: /, '')}`;
      logError(`workspace: delete of ${id.slice(0, 8)} stopped: ${e}`);
    }
  }

  // ── Hold to delete ──
  let holding = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;
  function startHold() {
    if (!canDelete || timer) return;
    holding = true;
    timer = setTimeout(() => { timer = null; holding = false; void perform(); }, HOLD_MS);
  }
  function stopHold() {
    if (!timer) return;
    clearTimeout(timer);
    timer = null;
    holding = false;
  }
  $effect(() => () => { if (timer) clearTimeout(timer); });

  // ── Placement and dismissal ──
  let el = $state<HTMLDivElement | null>(null);
  let top = $state(0);
  $effect(() => {
    if (!el) return;
    // Re-placed as the text grows (a Hive's preview arrives after the first paint).
    void lines.length; void refusal; void failure; void reading;
    const h = el.offsetHeight;
    top = anchor.bottom + h + 8 > window.innerHeight ? Math.max(8, anchor.top - h - 4) : anchor.bottom + 4;
  });
  // Focus Cancel two frames in: the menu that opened this is still unmounting on the first
  // frame, and the click that chose Delete lands on it.
  $effect(() => {
    let inner = 0;
    const outer = requestAnimationFrame(() => {
      inner = requestAnimationFrame(() => el?.querySelector<HTMLElement>('button.cancel')?.focus());
    });
    return () => { cancelAnimationFrame(outer); cancelAnimationFrame(inner); };
  });
  $effect(() => {
    const outside = (e: PointerEvent) => { if (!busy && el && !el.contains(e.target as Node)) close(); };
    // Escape closes it wherever focus is: a terminal that took focus back must not leave it stuck.
    const escape = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || busy) return;
      e.preventDefault();
      e.stopPropagation();
      close();
    };
    document.addEventListener('pointerdown', outside, true);
    window.addEventListener('keydown', escape, true);
    return () => {
      document.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('keydown', escape, true);
    };
  });
</script>

<div
  class="ws-delete"
  bind:this={el}
  role="alertdialog"
  aria-labelledby="ws-delete-title"
  tabindex="-1"
  style="left: {anchor.left + 6}px; width: {Math.max(200, anchor.width - 12)}px; top: {top}px"
>
  <div class="title" id="ws-delete-title">Delete {workspace.name}?</div>
  <ul>
    {#each lines as line (line)}<li>{line}</li>{/each}
  </ul>
  {#if reading}<p class="note">Reading what the Hive holds…</p>{/if}
  {#if refusal}<p class="error" role="alert">{refusal}</p>{/if}
  {#if failure}<p class="error" role="alert">{failure}</p>{/if}
  <div class="buttons">
    <button type="button" class="cancel" disabled={busy} onclick={close}>Cancel</button>
    <button
      type="button"
      class="hold"
      class:holding
      disabled={!canDelete}
      onpointerdown={(e) => { e.preventDefault(); startHold(); }}
      onpointerup={stopHold}
      onpointerleave={stopHold}
      onkeydown={(e) => { if ((e.key === 'Enter' || e.key === ' ') && !e.repeat) { e.preventDefault(); startHold(); } }}
      onkeyup={(e) => { if (e.key === 'Enter' || e.key === ' ') stopHold(); }}
      onblur={stopHold}
    >
      <span class="fill" style:--hold-ms="{HOLD_MS}ms"></span>
      <span class="label">{busy ? 'Deleting…' : holding ? 'Keep holding…' : 'Hold to delete'}</span>
    </button>
  </div>
</div>

<style>
  .ws-delete {
    background: var(--bg-medium);
    border: 1px solid var(--red);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    position: fixed;
    z-index: 1000;
  }
  .title { color: var(--fg); font-size: 0.9rem; font-weight: 600; overflow-wrap: anywhere; }
  ul { display: flex; flex-direction: column; gap: 3px; margin: 0; padding-left: 1.1em; }
  li, .note { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; overflow-wrap: anywhere; }
  .note { margin: 0; }
  .error { color: var(--red); font-size: 0.8rem; line-height: 1.4; margin: 0; overflow-wrap: anywhere; }

  .buttons { display: flex; gap: 6px; justify-content: flex-end; }
  button {
    border-radius: 5px;
    cursor: pointer;
    font-family: inherit;
    font-size: 0.8rem;
    padding: 5px 10px;
  }
  button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  button:disabled { cursor: default; opacity: 0.5; }
  .cancel { background: none; border: 1px solid var(--bg-light); color: var(--fg); }
  .hold {
    background: none;
    border: 1px solid var(--red);
    color: var(--red);
    overflow: hidden;
    position: relative;
    -webkit-user-select: none;
    user-select: none;
  }
  .hold .fill {
    background: var(--red);
    bottom: 0;
    left: 0;
    opacity: 0.35;
    position: absolute;
    top: 0;
    width: 0;
  }
  .hold.holding .fill { transition: width var(--hold-ms) linear; width: 100%; }
  .hold .label { position: relative; }
  @media (prefers-reduced-motion: reduce) {
    .hold.holding .fill { transition: none; width: 100%; }
  }
</style>
