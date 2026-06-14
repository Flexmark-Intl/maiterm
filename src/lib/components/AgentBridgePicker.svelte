<script lang="ts">
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { claudeStateStore } from '$lib/stores/claudeState.svelte';
  import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
  import { getPtyInfo } from '$lib/tauri/commands';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    open: boolean;
    /** The tab initiating the bridge (the active terminal). */
    callerTabId: string | null;
    onclose: () => void;
  }

  let { open, callerTabId, onclose }: Props = $props();

  interface Candidate {
    tabId: string;
    sessionId: string;
    tabName: string;
    workspaceName: string;
    cwd: string | null;
    state: 'active' | 'idle' | 'permission';
    /** Last Claude state change (ms) — for recency sorting. */
    lastActivity: number;
  }

  let selectedIndex = $state(0);
  let busy = $state(false);
  let errorMsg = $state<string | null>(null);
  // Free-text filter over tab name / workspace / cwd (for big agent fleets).
  let filter = $state('');
  let filterInput = $state<HTMLInputElement | null>(null);
  // 'existing' = bridge directly to the chosen running tab, no new pane (default).
  // 'fork' = fork the chosen session into a new split beside the caller.
  let mode = $state<'fork' | 'existing'>('existing');
  // Human-written context about the peer, fed into the calling agent's opener so it
  // knows what the peer is for (instead of blindly firing questions).
  let purpose = $state('');

  // Enumerate every terminal tab that has a live Claude session, except the
  // caller itself and any tab already in a bridge.
  const candidates = $derived.by((): Candidate[] => {
    void agentBridgeStore.version; // re-evaluate when bridges change
    const out: Candidate[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type !== 'terminal') continue;
          if (tab.id === callerTabId) continue;
          // Only a LIVE bridge (partner tab still open) constrains selection. A dead
          // bridge — partner tab was closed — is effectively unbridged, so don't hide
          // the survivor (this is what previously made a tab invisible to the picker
          // after its old partner's tab was closed).
          if (agentBridgeStore.isBridgedToLivePartner(tab.id)) {
            // Fork mode: never fork a tab already in a live bridge. Existing mode: allow
            // re-selecting the caller's OWN partner (to repair a failed reconnect), but
            // not a tab live-bridged to a third agent.
            if (mode === 'fork') continue;
            if (agentBridgeStore.getPartnerTabId(tab.id) !== callerTabId) continue;
          }
          const cs = claudeStateStore.getState(tab.id);
          if (!cs) continue;
          const osc = terminalsStore.getOsc(tab.id);
          out.push({
            tabId: tab.id,
            sessionId: cs.sessionId,
            tabName: tab.name,
            workspaceName: ws.name,
            cwd: osc?.cwd ?? osc?.promptCwd ?? null,
            state: cs.state,
            lastActivity: cs.updatedAt,
          });
        }
      }
    }
    return out;
  });

  // What the list actually renders: most-recently-active first, narrowed by the filter.
  const visibleCandidates = $derived.by((): Candidate[] => {
    const q = filter.trim().toLowerCase();
    const list = q
      ? candidates.filter((c) =>
          c.tabName.toLowerCase().includes(q) ||
          c.workspaceName.toLowerCase().includes(q) ||
          (c.cwd?.toLowerCase().includes(q) ?? false))
      : candidates.slice();
    list.sort((a, b) => b.lastActivity - a.lastActivity);
    return list;
  });

  const callerName = $derived.by(() => {
    if (!callerTabId) return null;
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find((t) => t.id === callerTabId);
        if (tab) return tab.name;
      }
    }
    return null;
  });

  $effect(() => {
    if (open) {
      selectedIndex = 0;
      errorMsg = null;
      busy = false;
      mode = 'existing';
      purpose = '';
      filter = '';
    }
  });

  $effect(() => {
    if (selectedIndex >= visibleCandidates.length) selectedIndex = Math.max(0, visibleCandidates.length - 1);
  });

  // Auto-focus the filter so power users can type-to-narrow immediately. Arrow/Enter
  // still bubble to the dialog handler for navigation.
  $effect(() => {
    if (open && filterInput) filterInput.focus();
  });

  async function choose(c: Candidate) {
    if (busy || !callerTabId) return;
    busy = true;
    errorMsg = null;
    try {
      // Connect directly to the existing tab — no fork, no new pane.
      if (mode === 'existing') {
        const res = await agentBridgeStore.bridgeExistingTab(callerTabId, c.tabId, purpose);
        if (!res.ok) { errorMsg = res.error; busy = false; return; }
        onclose();
        return;
      }

      // Fork mode: SSH session? Capture its ssh command + remote cwd so the fork reconnects.
      let sshCommand: string | null = null;
      let remoteCwd: string | null = null;
      let cwd = c.cwd;
      const inst = terminalsStore.get(c.tabId);
      if (inst) {
        try {
          const info = await getPtyInfo(inst.ptyId);
          if (info.foreground_command) {
            sshCommand = info.foreground_command; // already cleaned by getPtyInfo
            remoteCwd = c.cwd; // OSC cwd is the remote cwd when SSH is active
            cwd = info.cwd ?? null; // local cwd to launch ssh from
          }
        } catch { /* pty gone; fall through local */ }
      }

      const res = await agentBridgeStore.establishBridge(callerTabId, {
        sessionId: c.sessionId,
        tabName: c.tabName,
        workspaceName: c.workspaceName,
        cwd,
        sshCommand,
        remoteCwd,
      }, purpose);
      if (!res.ok) {
        errorMsg = res.error;
        busy = false;
        return;
      }
      onclose();
    } catch (e) {
      logError(`AgentBridgePicker: ${e}`);
      errorMsg = String(e);
      busy = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
      return;
    }
    // While typing the description, let the textarea own arrows/Enter (newlines);
    // Cmd/Ctrl+Enter still bridges the current selection.
    if ((e.target as HTMLElement)?.tagName === 'TEXTAREA') {
      if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        const c = visibleCandidates[selectedIndex];
        if (c) void choose(c);
      }
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (visibleCandidates.length) selectedIndex = (selectedIndex + 1) % visibleCandidates.length;
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (visibleCandidates.length) selectedIndex = (selectedIndex - 1 + visibleCandidates.length) % visibleCandidates.length;
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const c = visibleCandidates[selectedIndex];
      if (c) void choose(c);
      return;
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }

  function shortCwd(cwd: string | null): string {
    if (!cwd) return '';
    return cwd.replace(/^\/Users\/[^/]+/, '~').replace(/^\/home\/[^/]+/, '~');
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="backdrop"
    onclick={handleBackdropClick}
    onkeydown={handleKeydown}
    role="dialog"
    aria-modal="true"
    tabindex="-1"
  >
    <div class="palette">
      <div class="header">
        <div class="title">Agent Bridge</div>
        <div class="subtitle">
          {#if mode === 'fork'}
            Fork another Claude session into a split beside
          {:else}
            Connect an already-running tab directly to
          {/if}
          {#if callerName}<strong>{callerName}</strong>{:else}this tab{/if}.
          The two agents can then talk to each other.
        </div>
        <div class="mode-toggle" role="radiogroup" aria-label="Bridge mode">
          <button
            class="mode-btn"
            class:active={mode === 'existing'}
            role="radio"
            aria-checked={mode === 'existing'}
            disabled={busy}
            onclick={() => { mode = 'existing'; }}
          >Connect existing tab</button>
          <button
            class="mode-btn"
            class:active={mode === 'fork'}
            role="radio"
            aria-checked={mode === 'fork'}
            disabled={busy}
            onclick={() => { mode = 'fork'; }}
          >Fork into new pane</button>
        </div>
      </div>

      <div class="purpose-field">
        <textarea
          bind:value={purpose}
          rows="2"
          disabled={busy}
          placeholder="Describe this agent for your agent (optional) — what's it an expert on? How should your agent use it?"
        ></textarea>
      </div>

      {#if errorMsg}
        <div class="error-banner">{errorMsg}</div>
      {/if}

      {#if callerTabId && candidates.length > 0}
        <div class="search-field">
          <input
            bind:this={filterInput}
            bind:value={filter}
            type="text"
            disabled={busy}
            placeholder="Filter agents by name, workspace, or path…"
            oninput={() => { selectedIndex = 0; }}
          />
        </div>
      {/if}

      <div class="results">
        {#if !callerTabId}
          <div class="status">Open this from a terminal tab running Claude.</div>
        {:else if candidates.length === 0}
          <div class="status">
            No other registered Claude agents found. An agent appears here once it has
            registered with maiTerm — i.e. it has made at least one tool call (or you ran
            <code>/maiterm init</code> in its tab). Start Claude in another tab, let it take
            one turn, then reopen this.
          </div>
        {:else if visibleCandidates.length === 0}
          <div class="status">No agents match “{filter}”.</div>
        {:else}
          {#each visibleCandidates as c, i (c.tabId)}
            <button
              class="result-item"
              class:selected={i === selectedIndex}
              disabled={busy}
              onclick={() => choose(c)}
              onmouseenter={() => { selectedIndex = i; }}
            >
              <span class="state-dot" class:active={c.state === 'active'} class:permission={c.state === 'permission'}></span>
              <span class="info">
                <span class="name-row">
                  <span class="tab-name">{c.tabName}</span>
                  <span class="ws-name">{c.workspaceName}</span>
                </span>
                {#if c.cwd}<span class="cwd" title={c.cwd}>{shortCwd(c.cwd)}</span>{/if}
              </span>
            </button>
          {/each}
        {/if}
      </div>

      <div class="footer">
        <span class="hint">↑↓ navigate · ↵ connect · esc close</span>
        {#if busy}
          <span class="hint">{mode === 'fork' ? 'forking session…' : 'connecting…'}</span>
        {:else if candidates.length > 0}
          <span class="hint">{visibleCandidates.length} of {candidates.length}</span>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: center;
    padding-top: 15vh;
    z-index: 1000;
  }

  .palette {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    width: 520px;
    max-height: 460px;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    align-self: flex-start;
  }

  .header {
    padding: 12px 14px 10px;
    border-bottom: 1px solid var(--bg-light);
  }

  .title {
    font-size: 1rem;
    font-weight: 600;
    color: var(--fg);
  }

  .subtitle {
    margin-top: 3px;
    font-size: 0.8rem;
    color: var(--fg-dim);
    line-height: 1.4;
  }

  .subtitle strong {
    color: var(--accent);
    font-weight: 600;
  }

  .mode-toggle {
    display: flex;
    gap: 4px;
    margin-top: 10px;
    padding: 3px;
    background: var(--bg-dark);
    border-radius: 6px;
    width: fit-content;
  }

  .mode-btn {
    padding: 4px 12px;
    font-size: 0.78rem;
    font-family: inherit;
    border: none;
    border-radius: 4px;
    background: none;
    color: var(--fg-dim);
    cursor: pointer;
  }

  .mode-btn:hover:not(:disabled) {
    color: var(--fg);
  }

  .mode-btn.active {
    background: var(--bg-light);
    color: var(--fg);
    font-weight: 600;
  }

  .mode-btn:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .purpose-field {
    padding: 8px 12px 0;
  }

  .purpose-field textarea {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    min-height: 38px;
    padding: 7px 9px;
    font-family: inherit;
    font-size: 0.8rem;
    line-height: 1.4;
    color: var(--fg);
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
  }

  .purpose-field textarea:focus {
    outline: none;
    border-color: var(--accent);
  }

  .purpose-field textarea::placeholder {
    color: var(--fg-dim);
  }

  .search-field {
    padding: 8px 12px 4px;
  }

  .search-field input {
    width: 100%;
    box-sizing: border-box;
    padding: 6px 9px;
    font-family: inherit;
    font-size: 0.82rem;
    color: var(--fg);
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
  }

  .search-field input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .search-field input::placeholder {
    color: var(--fg-dim);
  }

  .status code {
    background: var(--bg-dark);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.85em;
  }

  .error-banner {
    margin: 8px 12px 0;
    padding: 6px 10px;
    font-size: 0.8rem;
    color: var(--red, #f7768e);
    border: 1px solid var(--red, #f7768e);
    border-radius: 4px;
    background: color-mix(in srgb, var(--red, #f7768e) 12%, transparent);
  }

  .results {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0;
  }

  .status {
    padding: 18px 14px;
    color: var(--fg-dim);
    font-size: 0.9rem;
    text-align: center;
    line-height: 1.5;
  }

  .result-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 14px;
    width: 100%;
    border: none;
    background: none;
    color: var(--fg);
    font-family: inherit;
    cursor: pointer;
    text-align: left;
  }

  .result-item:hover,
  .result-item.selected {
    background: var(--bg-light);
  }

  .result-item:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .state-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
    background: #9ece6a; /* idle = done/green */
  }

  .state-dot.active {
    background: var(--accent);
  }

  .state-dot.permission {
    background: #e0af68; /* needs attention = amber */
  }

  .info {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1;
  }

  .name-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }

  .tab-name {
    font-weight: 600;
    font-size: 0.9rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .ws-name {
    font-size: 0.75rem;
    color: var(--fg-dim);
    flex-shrink: 0;
  }

  .cwd {
    font-size: 0.78rem;
    color: var(--fg-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .footer {
    padding: 6px 14px;
    border-top: 1px solid var(--bg-light);
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .hint {
    font-size: 0.7rem;
    color: var(--fg-dim);
  }
</style>
