<script lang="ts">
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
  import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    open: boolean;
    workspaceId: string | null;
    onclose: () => void;
    onEnabled: (workspaceId: string) => void;
  }
  let { open, workspaceId, onclose, onEnabled }: Props = $props();

  type Status = 'ready' | 'not-registered' | 'suspended' | 'unnamed';
  const WAIT_TIMEOUT_MS = 30_000;

  // Tabs that have a pending action (init sent / wake fired), → start time. A row clears when
  // it reaches 'ready'; it flips to a timeout warning if it never comes online.
  let pending = $state<Record<string, number>>({});
  // Inline-rename buffer for unnamed tabs.
  let renaming = $state<Record<string, string>>({});
  let busy = $state(false);

  // 1s tick (only while open) so the inventory re-reads live/registration state + the waiter
  // timeouts advance without depending on every upstream store being individually reactive.
  let tick = $state(0);
  $effect(() => {
    if (!open) return;
    const id = setInterval(() => { tick++; }, 1000);
    return () => clearInterval(id);
  });

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
        const live = !!inst;
        const registered = !!claudeStateStore.getState(tab.id) || !!tab.runtime;
        const suspended = !!tab.pty_id && !live;
        if (!live && !suspended) continue; // never started / empty tab — not an agent
        const named = tab.custom_name === true;
        const role = roleName(tab.name);
        const status: Status = suspended ? 'suspended' : !named ? 'unnamed' : registered ? 'ready' : 'not-registered';
        out.push({ tabId: tab.id, paneId: pane.id, name: tab.name, role, status, live, hasResume: !!tab.auto_resume_command, ptyId: inst?.ptyId ?? null, generic: named && isGeneric(role) });
      }
    }
    return out;
  });

  const readyCount = $derived(rows.filter((r) => r.status === 'ready').length);
  const suspendedRows = $derived(rows.filter((r) => r.status === 'suspended'));
  const notRegistered = $derived(rows.filter((r) => r.status === 'not-registered'));
  // Duplicate role names (case-insensitive) among named tabs — peers fall back to handle.
  const dupNames = $derived.by(() => {
    const counts: Record<string, number> = {};
    for (const r of rows) if (r.status !== 'unnamed') counts[r.role.toLowerCase()] = (counts[r.role.toLowerCase()] ?? 0) + 1;
    return Object.entries(counts).filter(([, n]) => n > 1).map(([k]) => k);
  });

  // Per-row waiter state derived from `pending` + the tick. (Resolved entries are pruned in an
  // effect, not here — mutating state during render would loop.)
  function waitState(r: Row): 'waiting' | 'timeout' | null {
    const started = pending[r.tabId];
    if (started === undefined || r.status === 'ready') return null;
    void tick;
    return Date.now() - started > WAIT_TIMEOUT_MS ? 'timeout' : 'waiting';
  }

  // Prune pending entries once their tab reaches 'ready' (keeps the map from lingering).
  $effect(() => {
    const readyIds = new Set(rows.filter((r) => r.status === 'ready').map((r) => r.tabId));
    for (const id of Object.keys(pending)) if (readyIds.has(id)) delete pending[id];
  });

  async function sendInit(r: Row) {
    if (!r.ptyId) return;
    pending[r.tabId] = Date.now();
    try { await bracketedPasteSubmit(r.ptyId, '/maiterm init'); }
    catch (e) { logError(`mesh setup: send init failed for ${r.tabId.slice(0, 8)}: ${e}`); delete pending[r.tabId]; }
  }
  function wake(r: Row) {
    pending[r.tabId] = Date.now();
    window.dispatchEvent(new CustomEvent('mesh-activate-tab', { detail: r.tabId }));
  }
  function wakeAll() {
    for (const r of suspendedRows) wake(r);
  }
  function initAll() {
    for (const r of notRegistered) void sendInit(r);
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
    return s === 'ready' ? 'Ready' : s === 'not-registered' ? 'Not registered' : s === 'suspended' ? 'Suspended' : 'Needs a name';
  }
  function dotColor(s: Status): 'green' | 'yellow' | 'dim' {
    return s === 'ready' ? 'green' : s === 'suspended' ? 'dim' : 'yellow';
  }

  function handleKeydown(e: KeyboardEvent) { if (e.key === 'Escape') { e.stopPropagation(); onclose(); } }
  function handleBackdrop(e: MouseEvent) { if (e.target === e.currentTarget) onclose(); }
  const wsName = $derived(workspacesStore.workspaces.find((w) => w.id === workspaceId)?.name ?? 'this workspace');
</script>

{#if open && workspaceId}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="backdrop" onclick={handleBackdrop} onkeydown={handleKeydown} role="dialog" aria-modal="true" tabindex="-1">
    <div class="modal">
      <header>
        <span class="mesh-badge">MESH</span>
        <h2>Set up mesh — {wsName}</h2>
        <button class="close-btn" onclick={onclose} aria-label="Close">×</button>
      </header>
      <p class="sub">Each agent tab below joins the mesh once it's <strong>named</strong> and <strong>registered</strong> (has run <code>/maiterm init</code>). Fix any that aren't ready, then enable.</p>

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
                <span class="timeout" title="Didn't come online in 30s — check the tab">no response</span>
              {:else if r.status === 'not-registered'}
                <button class="mini" onclick={() => sendInit(r)} disabled={!r.ptyId}>Send init</button>
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
      {#if suspendedRows.length > 1 || notRegistered.length > 1}
        <div class="batch">
          {#if suspendedRows.length > 1}<button class="mini ghost" onclick={wakeAll}>Wake all suspended ({suspendedRows.length})</button>{/if}
          {#if notRegistered.length > 1}<button class="mini ghost" onclick={initAll}>Send init to all ({notRegistered.length})</button>{/if}
        </div>
      {/if}

      <!-- Warnings (non-blocking) -->
      {#if readyCount < 2 || dupNames.length > 0}
        <div class="warnings">
          {#if readyCount < 2}<div class="warn">⚠ A mesh needs at least 2 ready agents (you have {readyCount}).</div>{/if}
          {#each dupNames as n}<div class="warn">⚠ Two agents named "{n}" — peers will address them by handle, not name.</div>{/each}
        </div>
      {/if}

      <footer>
        <button class="mini ghost" onclick={onclose}>Cancel</button>
        <button class="primary" disabled={busy || readyCount === 0} onclick={enableMesh}>
          Enable Mesh{readyCount > 0 ? ` (${readyCount} ready)` : ''}
        </button>
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
  .status-tag.not-registered, .status-tag.unnamed { color: var(--yellow); }
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

  footer { display: flex; gap: 8px; justify-content: flex-end; padding: 12px 16px; border-top: 1px solid var(--bg-light); margin-top: 8px; }
  .primary { background: var(--accent); color: var(--bg-dark); border: none; border-radius: 5px; padding: 7px 16px; font-size: 12px; font-weight: 600; cursor: pointer; }
  .primary:hover { background: var(--accent-hover); }
  .primary:disabled { opacity: 0.5; cursor: default; }
  .mini { background: var(--accent); color: var(--bg-dark); border: none; border-radius: 4px; padding: 3px 9px; font-size: 11px; font-weight: 600; cursor: pointer; }
  .mini:hover { background: var(--accent-hover); }
  .mini:disabled { opacity: 0.5; cursor: default; }
  .mini.ghost { background: none; color: var(--fg-dim); border: 1px solid var(--bg-light); }
  .mini.ghost:hover { color: var(--fg); border-color: var(--fg-dim); }
</style>
