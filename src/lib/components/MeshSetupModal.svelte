<script lang="ts">
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
  import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
  import { getAgentLiveness } from '$lib/tauri/commands';
  import { replayAutoResume } from '$lib/stores/triggers.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    open: boolean;
    workspaceId: string | null;
    onclose: () => void;
    onEnabled: (workspaceId: string) => void;
  }
  let { open, workspaceId, onclose, onEnabled }: Props = $props();

  type Status = 'ready' | 'not-registered' | 'needs-init' | 'dropped' | 'suspended' | 'unnamed';
  const WAIT_TIMEOUT_MS = 30_000;
  // Retry cadence for pending inits on 'needs-init' rows. The first paste can be swallowed
  // whole by a startup dialog — e.g. Claude Code's "restore as is / compact first" resume
  // prompt, where the submit CR answers the DIALOG (starting a long compaction) and the init
  // text is lost. Re-sending until the tab registers delivers the init after the dialog
  // resolves; extra sends into a busy TUI are harmless (queued or dropped, and idempotent).
  const INIT_RETRY_MS = 20_000;
  const MAX_INIT_SENDS = 6; // initial send + 5 retries (~2 min — covers a long compaction)

  // Tabs that have a pending action (init sent / wake fired) → last-send time + send count.
  // A row clears when it reaches 'ready'; it flips to a timeout warning once retries are
  // exhausted (init) or the single attempt goes unanswered (wake/resume).
  let pending = $state<Record<string, { started: number; sends: number }>>({});
  // Inline-rename buffer for unnamed tabs.
  let renaming = $state<Record<string, string>>({});
  let busy = $state(false);

  // Process-based liveness probe per tab (async). A tab that WAS an agent but has no live
  // session right now is ambiguous: either the agent is still running and just lost its maiTerm
  // registration (e.g. its SessionStart hook raced the SSH bridge after an app restart) — needs
  // only `/maiterm init` — or the agent truly exited and the shell is back at a prompt — needs a
  // full auto-resume replay. `get_agent_liveness` answers this from ground truth: a claude/codex/
  // gemini process alive in the local tree, or a live ssh foreground (remote agent lives past the
  // hop). Either → running. Biasing toward init when in doubt is safe: worst case is a harmless
  // `/maiterm init` typed at a shell, vs. injecting ssh+resume into a running agent.
  let running = $state<Record<string, boolean>>({});

  async function refreshLiveness() {
    const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
    if (!ws) return;
    const seen = new Set<string>();
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if ((tab.tab_type ?? 'terminal') !== 'terminal') continue;
        const inst = terminalsStore.get(tab.id);
        if (!inst || !tab.runtime || claudeStateStore.getState(tab.id)) continue; // only ambiguous tabs
        seen.add(tab.id);
        try { const l = await getAgentLiveness(inst.ptyId); running[tab.id] = l.agent_running || l.ssh_foreground; }
        catch { /* PTY may have just closed — keep the prior reading */ }
      }
    }
    // Drop tabs that are no longer ambiguous so a stale `true` can't linger as a false "running".
    for (const id of Object.keys(running)) if (!seen.has(id)) delete running[id];
  }

  // 1s tick (only while open) so the inventory re-reads live/registration state + the waiter
  // timeouts advance without depending on every upstream store being individually reactive.
  // The same tick refreshes the liveness probe so running-but-unregistered agents are caught.
  let tick = $state(0);
  $effect(() => {
    if (!open) return;
    void refreshLiveness();
    const id = setInterval(() => { tick++; void refreshLiveness(); retryPendingInits(); }, 1000);
    return () => clearInterval(id);
  });

  // Re-send `/maiterm init` to rows that are still running-but-unregistered after a send —
  // see INIT_RETRY_MS for why the first paste can vanish (startup dialogs eat it). Runs from
  // the interval (not an effect) so the pending-map writes can't re-trigger reactivity.
  function retryPendingInits() {
    for (const r of rows) {
      const p = pending[r.tabId];
      if (!p || r.status !== 'needs-init' || !r.ptyId) continue;
      if (p.sends >= MAX_INIT_SENDS || Date.now() - p.started < INIT_RETRY_MS) continue;
      p.started = Date.now();
      p.sends += 1;
      void bracketedPasteSubmit(r.ptyId, '/maiterm init')
        .catch((e) => logError(`mesh setup: init retry failed for ${r.tabId.slice(0, 8)}: ${e}`));
    }
  }

  function roleName(name: string): string {
    return name.replace(/^[⇄↔→⌗]\s*/u, '').trim() || 'agent';
  }
  function isGeneric(role: string): boolean {
    return /^(zsh|bash|sh|fish|terminal|node|claude|codex|gemini|shell|untitled|tab\s*\d+)\b/i.test(role);
  }

  interface Row {
    tabId: string; paneId: string; name: string; role: string;
    status: Status; live: boolean; hasResume: boolean; ptyId: string | null; generic: boolean;
  }

  const rows = $derived.by((): Row[] => {
    void agentMeshStore.version; void tick;
    const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
    if (!ws) return [];
    const out: Row[] = [];
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if ((tab.tab_type ?? 'terminal') !== 'terminal') continue;
        const inst = terminalsStore.get(tab.id);
        const termLive = !!inst;                                // terminal/PTY attached
        const agentLive = !!claudeStateStore.getState(tab.id);  // agent SESSION running + init'd
        const wasAgent = !!tab.runtime;                         // persisted: this tab was an agent
        const suspended = !!tab.pty_id && !termLive;
        if (!termLive && !suspended) continue; // never started / empty tab — not an agent
        const named = tab.custom_name === true;
        const role = roleName(tab.name);
        // "Ready" REQUIRES a live agent session — the same signal that lights the tab's status
        // dot (claudeState exists only between SessionStart and SessionEnd). `tab.runtime`
        // persists across restarts, so it proves the tab WAS an agent, not that one is running
        // now. A tab with runtime but no live session splits two ways by the liveness probe:
        //  • agent process still running (or a live remote ssh session) → it's just unregistered
        //    (hook lost after restart) → 'needs-init' (offer `/maiterm init`, non-destructive);
        //  • back at a shell prompt → the agent really exited → 'dropped' (offer full Resume).
        const status: Status =
          suspended ? 'suspended'
          : !named ? 'unnamed'
          : agentLive ? 'ready'
          : wasAgent ? (running[tab.id] ? 'needs-init' : 'dropped')
          : 'not-registered';
        out.push({ tabId: tab.id, paneId: pane.id, name: tab.name, role, status, live: termLive, hasResume: !!tab.auto_resume_command, ptyId: inst?.ptyId ?? null, generic: named && isGeneric(role) });
      }
    }
    return out;
  });

  const readyCount = $derived(rows.filter((r) => r.status === 'ready').length);
  const suspendedRows = $derived(rows.filter((r) => r.status === 'suspended'));
  const droppedRows = $derived(rows.filter((r) => r.status === 'dropped'));
  // Tabs whose fix is `/maiterm init`: never-registered shells AND running-but-unregistered
  // agents (a live agent that just needs to re-announce its tab — no resume replay).
  const initableRows = $derived(rows.filter((r) => r.status === 'not-registered' || r.status === 'needs-init'));
  // Duplicate role names (case-insensitive) among named tabs — peers fall back to handle.
  const dupNames = $derived.by(() => {
    const counts: Record<string, number> = {};
    for (const r of rows) if (r.status !== 'unnamed') counts[r.role.toLowerCase()] = (counts[r.role.toLowerCase()] ?? 0) + 1;
    return Object.entries(counts).filter(([, n]) => n > 1).map(([k]) => k);
  });

  // Per-row waiter state derived from `pending` + the tick. (Resolved entries are pruned in an
  // effect, not here — mutating state during render would loop.)
  function waitState(r: Row): 'waiting' | 'timeout' | null {
    const p = pending[r.tabId];
    if (p === undefined || r.status === 'ready') return null;
    void tick;
    // needs-init rows keep waiting while retries remain (see retryPendingInits).
    const canRetry = r.status === 'needs-init' && p.sends < MAX_INIT_SENDS;
    return !canRetry && Date.now() - p.started > WAIT_TIMEOUT_MS ? 'timeout' : 'waiting';
  }

  // Prune pending entries once their tab reaches 'ready' (keeps the map from lingering).
  $effect(() => {
    const readyIds = new Set(rows.filter((r) => r.status === 'ready').map((r) => r.tabId));
    for (const id of Object.keys(pending)) if (readyIds.has(id)) delete pending[id];
  });

  async function sendInit(r: Row) {
    if (!r.ptyId) return;
    pending[r.tabId] = { started: Date.now(), sends: 1 };
    try { await bracketedPasteSubmit(r.ptyId, '/maiterm init'); }
    catch (e) { logError(`mesh setup: send init failed for ${r.tabId.slice(0, 8)}: ${e}`); delete pending[r.tabId]; }
  }
  function wake(r: Row) {
    pending[r.tabId] = { started: Date.now(), sends: 1 };
    window.dispatchEvent(new CustomEvent('mesh-activate-tab', { detail: r.tabId }));
  }
  // Dropped = live shell, agent gone (auto-resume failed/ended). Re-run the tab's auto-resume
  // in its live PTY (same path as the tab context-menu "replay auto-resume" — handles SSH, cwd,
  // and %var interpolation). The agent re-registers + rejoins once its session comes back up.
  async function resumeDropped(r: Row) {
    if (!r.hasResume) return;
    pending[r.tabId] = { started: Date.now(), sends: 1 };
    try { await replayAutoResume(r.tabId); }
    catch (e) { logError(`mesh setup: resume failed for ${r.tabId.slice(0, 8)}: ${e}`); delete pending[r.tabId]; }
  }
  function wakeAll() {
    for (const r of suspendedRows) wake(r);
  }
  function initAll() {
    for (const r of initableRows) void sendInit(r);
  }
  function resumeAllDropped() {
    for (const r of droppedRows) if (r.hasResume) void resumeDropped(r);
  }
  function startRename(r: Row) {
    renaming[r.tabId] = r.role === 'agent' ? '' : r.role;
  }
  async function saveRename(r: Row) {
    const name = (renaming[r.tabId] ?? '').trim();
    if (!name) return;
    await workspacesStore.renameTab(workspaceId!, r.paneId, r.tabId, name, true);
    delete renaming[r.tabId];
  }
  async function enableMesh() {
    if (!workspaceId) return;
    busy = true;
    try {
      await agentMeshStore.setMeshEnabled(workspaceId, true);
      onEnabled(workspaceId);
      onclose();
    } finally { busy = false; }
  }

  function statusLabel(s: Status): string {
    return s === 'ready' ? 'Ready'
      : s === 'not-registered' ? 'Not registered'
      : s === 'needs-init' ? 'Running · needs init'
      : s === 'dropped' ? 'Dropped'
      : s === 'suspended' ? 'Suspended'
      : 'Needs a name';
  }
  function dotColor(s: Status): 'green' | 'yellow' | 'red' | 'dim' {
    return s === 'ready' ? 'green' : s === 'dropped' ? 'red' : s === 'suspended' ? 'dim' : 'yellow';
  }

  function handleKeydown(e: KeyboardEvent) { if (e.key === 'Escape') { e.stopPropagation(); onclose(); } }
  function handleBackdrop(e: MouseEvent) { if (e.target === e.currentTarget) onclose(); }
  const wsObj = $derived(workspacesStore.workspaces.find((w) => w.id === workspaceId));
  const wsName = $derived(wsObj?.name ?? 'this workspace');
  // Re-check mode: the workspace is ALREADY a mesh (e.g. reopened after restart). Then the
  // primary action is just "Done" — woken/init'd agents auto-prime + join on their own, so
  // there's nothing to "enable". Otherwise this is first-time setup.
  const alreadyMesh = $derived(!!wsObj?.bridge_all);
</script>

{#if open && workspaceId}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="backdrop" onclick={handleBackdrop} onkeydown={handleKeydown} role="dialog" aria-modal="true" tabindex="-1">
    <div class="modal">
      <header>
        <span class="mesh-badge">MESH</span>
        <h2>{alreadyMesh ? 'Mesh readiness' : 'Set up mesh'} — {wsName}</h2>
        <button class="close-btn" onclick={onclose} aria-label="Close">×</button>
      </header>
      <p class="sub">
        Each agent tab joins once it's <strong>named</strong> and <strong>registered</strong> (has run <code>/maiterm init</code>).
        {#if alreadyMesh}Wake or re-init any that dropped (e.g. after an app restart) — they rejoin and re-prime on their own.{:else}Fix any that aren't ready, then enable.{/if}
      </p>

      {#if rows.length === 0}
        <div class="empty">No terminal tabs in this workspace yet. Open agent tabs first.</div>
      {/if}

      <div class="rows">
        {#each rows as r (r.tabId)}
          {@const w = waitState(r)}
          <div class="row" class:ready={r.status === 'ready'}>
            <StatusDot color={dotColor(r.status)} pulse={w === 'waiting'} />
            <div class="meta">
              {#if r.status === 'unnamed'}
                <div class="rename">
                  <input
                    placeholder="descriptive name (its address)…"
                    bind:value={renaming[r.tabId]}
                    onfocus={() => startRename(r)}
                    onkeydown={(e) => { if (e.key === 'Enter') saveRename(r); }}
                  />
                  <button class="mini" onclick={() => saveRename(r)}>Name it</button>
                </div>
              {:else}
                <span class="role">{r.role}</span>
                {#if r.generic}<span class="nudge" title="A generic name is a poor address — rename for clarity">generic name</span>{/if}
              {/if}
              <span class="status-tag {r.status}">{statusLabel(r.status)}</span>
            </div>
            <div class="action">
              {#if w === 'waiting'}
                <span class="waiting">waiting…</span>
              {:else if w === 'timeout'}
                <span class="timeout" title="Didn't come online — check the tab">no response</span>
                {#if r.status === 'not-registered' || r.status === 'needs-init'}
                  <button class="mini" onclick={() => sendInit(r)} disabled={!r.ptyId}>Retry</button>
                {/if}
              {:else if r.status === 'not-registered' || r.status === 'needs-init'}
                <button class="mini" onclick={() => sendInit(r)} disabled={!r.ptyId}>Send init</button>
              {:else if r.status === 'dropped'}
                {#if r.hasResume}
                  <button class="mini" onclick={() => resumeDropped(r)} disabled={!r.ptyId}>Resume</button>
                {:else}
                  <span class="warn-inline" title="No auto-resume command — restart the agent in its tab">restart in tab</span>
                {/if}
              {:else if r.status === 'suspended'}
                <button class="mini" onclick={() => wake(r)}>Wake</button>
              {/if}
              {#if r.status === 'suspended' && !r.hasResume}
                <span class="warn-inline" title="No auto-resume configured — this wakes as a bare shell, not the agent">no resume</span>
              {/if}
            </div>
          </div>
        {/each}
      </div>

      <!-- Batch actions -->
      {#if suspendedRows.length > 1 || initableRows.length > 1 || droppedRows.length > 1}
        <div class="batch">
          {#if suspendedRows.length > 1}<button class="mini ghost" onclick={wakeAll}>Wake all suspended ({suspendedRows.length})</button>{/if}
          {#if droppedRows.length > 1}<button class="mini ghost" onclick={resumeAllDropped}>Resume all dropped ({droppedRows.length})</button>{/if}
          {#if initableRows.length > 1}<button class="mini ghost" onclick={initAll}>Send init to all ({initableRows.length})</button>{/if}
        </div>
      {/if}

      <!-- Warnings (non-blocking) -->
      {#if readyCount < 2 || dupNames.length > 0 || droppedRows.length > 0}
        <div class="warnings">
          {#if droppedRows.length > 0}<div class="warn">⚠ {droppedRows.length} agent{droppedRows.length === 1 ? '' : 's'} dropped — auto-resume didn't bring {droppedRows.length === 1 ? 'it' : 'them'} back. Click Resume (or check the tab).</div>{/if}
          {#if readyCount < 2}<div class="warn">⚠ A mesh needs at least 2 ready agents (you have {readyCount}).</div>{/if}
          {#each dupNames as n}<div class="warn">⚠ Two agents named "{n}" — peers will address them by handle, not name.</div>{/each}
        </div>
      {/if}

      <footer>
        {#if alreadyMesh}
          <span class="ready-count">{readyCount} agent{readyCount === 1 ? '' : 's'} ready</span>
          <button class="primary" onclick={onclose}>Done</button>
        {:else}
          <button class="mini ghost" onclick={onclose}>Cancel</button>
          <button class="primary" disabled={busy || readyCount === 0} onclick={enableMesh}>
            Enable Mesh{readyCount > 0 ? ` (${readyCount} ready)` : ''}
          </button>
        {/if}
      </footer>
    </div>
  </div>
{/if}

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0,0,0,0.45); z-index: 1000; display: flex; align-items: center; justify-content: center; }
  .modal { width: 560px; max-width: 92vw; max-height: 86vh; overflow-y: auto; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 10px; box-shadow: 0 12px 40px rgba(0,0,0,0.5); display: flex; flex-direction: column; }
  header { display: flex; align-items: center; gap: 8px; padding: 14px 16px 8px; }
  header h2 { font-size: 14px; margin: 0; font-weight: 600; color: var(--fg); }
  .mesh-badge { font-size: 9px; font-weight: 700; letter-spacing: 0.08em; color: var(--bg-dark); background: var(--accent); padding: 2px 5px; border-radius: 3px; }
  .close-btn { margin-left: auto; background: none; border: none; color: var(--fg-dim); font-size: 20px; line-height: 1; cursor: pointer; }
  .close-btn:hover { color: var(--fg); }
  .sub { padding: 0 16px 8px; margin: 0; font-size: 12px; color: var(--fg-dim); line-height: 1.5; }
  .sub code { background: var(--bg-dark); padding: 1px 4px; border-radius: 3px; }
  .empty { padding: 24px 16px; color: var(--fg-dim); font-size: 12px; text-align: center; }

  .rows { padding: 4px 12px; display: flex; flex-direction: column; gap: 4px; }
  .row { display: flex; align-items: center; gap: 8px; padding: 7px 8px; background: var(--bg-dark); border-radius: 6px; }
  .row.ready { opacity: 0.85; }
  .meta { display: flex; align-items: center; gap: 8px; flex: 1; min-width: 0; }
  .role { font-size: 13px; font-weight: 600; color: var(--fg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .status-tag { font-size: 9px; text-transform: uppercase; letter-spacing: 0.05em; padding: 1px 5px; border-radius: 3px; flex-shrink: 0; }
  .status-tag.ready { color: var(--green); }
  .status-tag.not-registered, .status-tag.needs-init, .status-tag.unnamed { color: var(--yellow); }
  .status-tag.dropped { color: var(--red); }
  .status-tag.suspended { color: var(--fg-dim); }
  .nudge, .warn-inline { font-size: 9px; color: var(--yellow); border: 1px solid color-mix(in srgb, var(--yellow) 40%, transparent); padding: 0 4px; border-radius: 3px; flex-shrink: 0; }
  .action { display: flex; align-items: center; gap: 6px; flex-shrink: 0; }
  .waiting { font-size: 11px; color: var(--accent); }
  .timeout { font-size: 11px; color: var(--red); }

  .rename { display: flex; gap: 6px; flex: 1; }
  .rename input { flex: 1; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 4px; color: var(--fg); font-size: 12px; padding: 3px 6px; }
  .rename input:focus { outline: none; border-color: var(--accent); }

  .batch { display: flex; gap: 8px; padding: 6px 16px; }
  .warnings { padding: 4px 16px; display: flex; flex-direction: column; gap: 4px; }
  .warn { font-size: 11px; color: var(--yellow); }

  footer { display: flex; gap: 8px; align-items: center; justify-content: flex-end; padding: 12px 16px; border-top: 1px solid var(--bg-light); margin-top: 8px; }
  .ready-count { margin-right: auto; font-size: 11px; color: var(--fg-dim); }
  .primary { background: var(--accent); color: var(--bg-dark); border: none; border-radius: 5px; padding: 7px 16px; font-size: 12px; font-weight: 600; cursor: pointer; }
  .primary:hover { background: var(--accent-hover); }
  .primary:disabled { opacity: 0.5; cursor: default; }
  .mini { background: var(--accent); color: var(--bg-dark); border: none; border-radius: 4px; padding: 3px 9px; font-size: 11px; font-weight: 600; cursor: pointer; }
  .mini:hover { background: var(--accent-hover); }
  .mini:disabled { opacity: 0.5; cursor: default; }
  .mini.ghost { background: none; color: var(--fg-dim); border: 1px solid var(--bg-light); }
  .mini.ghost:hover { color: var(--fg); border-color: var(--fg-dim); }
</style>
