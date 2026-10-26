<!--
  An agent's request only the human can approve (docs/agent-requests.md): a mesh change, a move
  that changes a tab's mesh, or restarting maiTerm. The Decisions queue draws one per request.

  The question and its consequences are maiTerm's own words, built from maiTerm's state when the
  request was filed; only the reason is the agent's, and it is shown as a quote. The approval is
  for exactly what is drawn here: the window carries it out from the stored request, never from
  anything the agent sends later. This card is the only caller of `agentRequestsStore.decide`.
-->
<script lang="ts">
  import { agentRequestsStore } from '$lib/stores/agentRequests.svelte';
  import { workspacesStore, tabDisplayName } from '$lib/stores/workspaces.svelte';
  import type { AgentRequest } from '$lib/tauri/types';

  interface Props {
    request: AgentRequest;
    /** Receives one line saying what happened, for the surface to show. */
    onnote?: (text: string) => void;
    /** When the list holding this card last changed (ms): anything above it moving can put a
     *  card under the pointer (ScriptApprovalCard's rule). */
    listChangedAt?: number;
  }
  let { request: r, onnote, listChangedAt = 0 }: Props = $props();

  /** Nothing is decided in a card's first moments, counted from the latest of: the agent asking,
   *  this card appearing, and its list last changing. */
  const FRESH_MS = 1500;
  const shownAt = Date.now();
  const isFresh = () => {
    const age = Date.now() - r.requested_at;
    const settled = Date.now() - Math.max(shownAt, listChangedAt);
    return (age >= 0 && age < FRESH_MS) || settled < FRESH_MS;
  };
  const secondClick = (e: MouseEvent) => e.detail > 1;

  let busy = $state(false);

  /** Who asked: the tab's name where this window has it. */
  const asker = $derived.by(() => {
    for (const ws of workspacesStore.workspaces) {
      for (const p of ws.panes) {
        const t = p.tabs.find((x) => x.id === r.tab_id);
        if (t) return `${tabDisplayName(t.id)} · ${ws.name}`;
      }
    }
    // Another window's tab, or one closed since: as it read when it asked.
    return r.asker_workspace ? `${r.asker_title} · ${r.asker_workspace}` : r.asker_title;
  });

  const heading = $derived(
    r.kind === 'restart' ? 'Allow this restart?'
      : r.kind === 'tab_create' ? 'Allow this new agent?'
        : r.kind === 'bridge_fork' ? 'Allow this new agent and bridge?'
          : r.kind === 'mcp_add' ? 'Add this MCP server for every agent?'
          : r.kind === 'tab_reload' ? 'Allow this reload?'
          : r.kind === 'window_reload' ? 'Reload this window?'
          : r.kind === 'hive_land' ? 'Allow this land?'
          : r.kind === 'hive_undo' ? 'Allow this undo?'
          : r.kind === 'hive_settings' ? 'Change these Hive settings?'
          : r.kind.startsWith('bridge') ? 'Allow this bridge change?'
            : r.kind.startsWith('mesh') ? 'Allow this mesh change?' : 'Allow this move?',
  );

  async function decide(approve: boolean, e: MouseEvent) {
    if (busy || secondClick(e)) return;
    if (isFresh()) {
      onnote?.('This just appeared or moved. Read it, then choose.');
      return;
    }
    busy = true;
    try {
      const done = await agentRequestsStore.decide(r.id, approve);
      onnote?.(
        !done
          ? 'It was already answered, or it lapsed.'
          : approve
            ? r.kind === 'restart' ? 'Approved. maiTerm is restarting.'
              : r.kind === 'hive_land' ? 'Approved. maiTerm lands it now, and the coordinator will be told.'
              : r.kind === 'hive_settings' ? 'Approved. The settings are changed, and the coordinator will be told.'
              : 'Approved. It is being carried out, and the agent will be told.'
            : 'Denied. The agent will be told.',
      );
    } catch (err) {
      onnote?.(`Could not save that: ${err}`);
    } finally {
      busy = false;
    }
  }
</script>

<div class="request">
  <p class="q">{heading}</p>
  <p class="what">{r.summary}</p>
  {#if r.effects.length}
    <ul class="effects">
      {#each r.effects as line, i (i)}<li>{line}</li>{/each}
    </ul>
  {/if}
  <p class="who">Asked by <strong>{asker}</strong></p>
  {#if r.reason}
    <blockquote class="reason">{r.reason}</blockquote>
  {/if}
  <div class="choices">
    <button class="allow" disabled={busy} onclick={(e) => decide(true, e)}>Approve</button>
    <button class="deny" disabled={busy} onclick={(e) => decide(false, e)}>Deny</button>
  </div>
</div>

<style>
  .request {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border-left: 3px solid var(--yellow, #e0af68);
    background: color-mix(in srgb, var(--yellow, #e0af68) 7%, transparent);
    border-radius: 0 8px 8px 0;
    font-size: 12.5px;
    color: var(--fg);
    min-width: 0;
  }
  .q { margin: 0; font-size: 15px; font-weight: 600; line-height: 1.3; }
  .what { margin: 0; font-weight: 600; overflow-wrap: anywhere; line-height: 1.4; }
  .effects { margin: 0; padding-left: 18px; color: var(--fg); line-height: 1.45; }
  .effects li { overflow-wrap: anywhere; }
  .who { margin: 0; color: var(--fg-dim); overflow-wrap: anywhere; }
  .who strong { color: var(--fg); font-weight: 600; }
  /* The agent's words, set apart from maiTerm's own description above. */
  .reason {
    margin: 0;
    padding: 4px 10px;
    border-left: 2px solid var(--bg-light);
    color: var(--fg-dim);
    font-style: italic;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .choices { display: flex; flex-wrap: wrap; gap: 6px; }
  .choices button {
    font: inherit;
    font-weight: 600;
    padding: 5px 14px;
    border-radius: 6px;
    cursor: pointer;
  }
  .allow { background: var(--accent); border: 1px solid var(--accent); color: var(--bg-dark); }
  .allow:hover:not(:disabled) { filter: brightness(1.1); }
  .deny { background: none; border: 1px solid var(--bg-light); color: var(--fg); }
  .deny:hover:not(:disabled) { background: var(--bg-light); }
  .choices button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .choices button:disabled { opacity: 0.5; cursor: default; }
</style>
