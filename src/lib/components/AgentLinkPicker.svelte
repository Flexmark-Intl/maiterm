<script lang="ts">
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { claudeStateStore } from '$lib/stores/claudeState.svelte';
  import { agentLinkStore } from '$lib/stores/agentLink.svelte';
  import { getPtyInfo } from '$lib/tauri/commands';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    open: boolean;
    /** The tab initiating the link (the active terminal). */
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
  }

  let selectedIndex = $state(0);
  let busy = $state(false);
  let errorMsg = $state<string | null>(null);

  // Enumerate every terminal tab that has a live Claude session, except the
  // caller itself and any tab already in a link.
  const candidates = $derived.by((): Candidate[] => {
    void agentLinkStore.version; // re-evaluate when links change
    const out: Candidate[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type !== 'terminal') continue;
          if (tab.id === callerTabId) continue;
          if (agentLinkStore.isLinked(tab.id)) continue;
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
          });
        }
      }
    }
    return out;
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
    }
  });

  $effect(() => {
    if (selectedIndex >= candidates.length) selectedIndex = Math.max(0, candidates.length - 1);
  });

  async function choose(c: Candidate) {
    if (busy || !callerTabId) return;
    busy = true;
    errorMsg = null;
    try {
      // SSH session? Capture its ssh command + remote cwd so the fork reconnects.
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

      const res = await agentLinkStore.establishLink(callerTabId, {
        sessionId: c.sessionId,
        tabName: c.tabName,
        workspaceName: c.workspaceName,
        cwd,
        sshCommand,
        remoteCwd,
      });
      if (!res.ok) {
        errorMsg = res.error;
        busy = false;
        return;
      }
      onclose();
    } catch (e) {
      logError(`AgentLinkPicker: ${e}`);
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
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (candidates.length) selectedIndex = (selectedIndex + 1) % candidates.length;
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (candidates.length) selectedIndex = (selectedIndex - 1 + candidates.length) % candidates.length;
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const c = candidates[selectedIndex];
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
        <div class="title">Link to Agent</div>
        <div class="subtitle">
          Fork another Claude session into a split beside
          {#if callerName}<strong>{callerName}</strong>{:else}this tab{/if}.
          The two agents can then talk to each other.
        </div>
      </div>

      {#if errorMsg}
        <div class="error-banner">{errorMsg}</div>
      {/if}

      <div class="results">
        {#if !callerTabId}
          <div class="status">Open this from a terminal tab running Claude.</div>
        {:else if candidates.length === 0}
          <div class="status">
            No other Claude sessions found. Start Claude in another tab, then try again.
          </div>
        {:else}
          {#each candidates as c, i (c.tabId)}
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
        <span class="hint">↑↓ navigate · ↵ link · esc close</span>
        {#if busy}<span class="hint">forking session…</span>{/if}
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
