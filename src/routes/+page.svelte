<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
  import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import WorkspaceSidebar from '$lib/components/workspace/WorkspaceSidebar.svelte';
  import SplitContainer from '$lib/components/pane/SplitContainer.svelte';
  import MeshStageView from '$lib/components/MeshStageView.svelte';
  import TerminalPane from '$lib/components/terminal/TerminalPane.svelte';
  import EditorPane from '$lib/components/editor/EditorPane.svelte';
  import DiffPane from '$lib/components/editor/DiffPane.svelte';
  import ChangelogModal from '$lib/components/ChangelogModal.svelte';
  import { navHistoryStore } from '$lib/stores/navHistory.svelte';
  import { pendingResumePanes, resumePane } from '$lib/stores/resumeGate.svelte';
  import Resizer from '$lib/components/Resizer.svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { modLabel, modSymbol, altLabel } from '$lib/utils/platform';
  import * as commands from '$lib/tauri/commands';

  let loading = $state(true);
  let showChangelog = $state(false);
  let appVersion = $state('');
  getVersion().then(v => { appVersion = v; });

  // Track which workspaces have been visited so we lazily mount terminals
  // on first activation but keep them alive across workspace switches.
  const activatedWorkspaceIds = new SvelteSet<string>();

  // Track which terminal tabs have been activated (became the active tab
  // while their workspace was visible). PTYs only spawn on first activation,
  // preventing idle tabs from accumulating bash processes and reader threads.
  const activatedTabIds = new SvelteSet<string>();

  let lastActiveWorkspaceId: string | null = null;
  // Skip resume gate on the very first workspace activation (app startup/restore).
  let initialActivationDone = false;

  $effect.pre(() => {
    const id = workspacesStore.activeWorkspaceId;

    // Read workspace structure outside untrack() so the effect re-runs when
    // panes, tabs, or active_tab_id change via fine-grained Svelte 5 reactivity.
    // Only SvelteSet mutations (activatedTabIds, activatedWorkspaceIds, pendingResumePanes)
    // stay inside untrack() to avoid effect_update_depth_exceeded.
    const ws = workspacesStore.workspaces.find(w => w.id === id);
    const paneSnapshots = ws?.panes.map(p => ({
      id: p.id,
      active_tab_id: p.active_tab_id,
      tabs: p.tabs,
    })) ?? [];

    // Snapshot suspended workspaces for cleanup
    const suspendedSnapshots: { id: string; panes: { id: string; tabIds: string[] }[] }[] = [];
    for (const w of workspacesStore.workspaces) {
      if (w.suspended) {
        suspendedSnapshots.push({
          id: w.id,
          panes: w.panes.map(p => ({ id: p.id, tabIds: p.tabs.map(t => t.id) })),
        });
      }
    }

    untrack(() => {
      const workspaceSwitched = id !== lastActiveWorkspaceId;
      if (id) {
        activatedWorkspaceIds.add(id);
        lastActiveWorkspaceId = id;
      }

      // Activate the current active tab in each pane of the active workspace.
      // Uses $effect.pre so activatedTabIds is updated before DOM render,
      // avoiding a frame where the tab slot is empty.
      // On workspace switch (not initial load), suspended tabs show a resume prompt.
      for (const paneSnap of paneSnapshots) {
        const tabId = paneSnap.active_tab_id;
        if (!tabId) continue;
        const tab = paneSnap.tabs.find(t => t.id === tabId);
        const isTerminal = tab && (tab.tab_type === 'terminal' || !tab.tab_type);
        // Only treat as suspended if the tab previously had a PTY (pty_id set but no live instance).
        // Brand-new tabs have pty_id === null and should activate immediately.
        const isSuspended = isTerminal && !!tab?.pty_id && !terminalsStore.get(tabId) && !activatedTabIds.has(tabId);

        if (initialActivationDone && workspaceSwitched && isSuspended) {
          // Workspace switch landed on a suspended tab — show resume prompt
          pendingResumePanes.add(paneSnap.id);
        } else if (pendingResumePanes.has(paneSnap.id) && isSuspended) {
          // Pane is pending resume and new active tab is also suspended — keep waiting
        } else if (pendingResumePanes.has(paneSnap.id)) {
          // User clicked a non-suspended tab within a pending-resume pane — activate it
          activatedTabIds.add(tabId);
          pendingResumePanes.delete(paneSnap.id);
        } else {
          activatedTabIds.add(tabId);
        }
      }
      if (workspaceSwitched) initialActivationDone = true;

      // Clean up suspended workspaces — remove from activated sets so their
      // TerminalPane/EditorPane/DiffPane components get destroyed, freeing resources.
      for (const snap of suspendedSnapshots) {
        if (activatedWorkspaceIds.has(snap.id)) {
          activatedWorkspaceIds.delete(snap.id);
          if (snap.id === lastActiveWorkspaceId) lastActiveWorkspaceId = null;
          for (const pane of snap.panes) {
            for (const tabId of pane.tabIds) {
              activatedTabIds.delete(tabId);
            }
            pendingResumePanes.delete(pane.id);
          }
        }
      }
    });
  });

  // When a pending-resume pane is resolved (user clicked resume or a tab),
  // listen for resumePane clearing the set and activate the current tab.
  $effect.pre(() => {
    const id = workspacesStore.activeWorkspaceId;
    // Re-run when pendingResumePanes changes size
    void pendingResumePanes.size;

    // Read pane data outside untrack() for fine-grained reactivity
    const ws = workspacesStore.workspaces.find(w => w.id === id);
    const paneData = ws?.panes.map(p => ({ id: p.id, active_tab_id: p.active_tab_id })) ?? [];

    untrack(() => {
      for (const p of paneData) {
        if (p.active_tab_id && !pendingResumePanes.has(p.id)) {
          activatedTabIds.add(p.active_tab_id);
        }
      }
    });
  });

  // Auto-suspend: periodically check for inactive workspaces
  $effect(() => {
    const minutes = preferencesStore.autoSuspendMinutes;
    if (!minutes) return;

    const interval = setInterval(() => {
      const cutoff = Date.now() - minutes * 60 * 1000;
      for (const ws of workspacesStore.workspaces) {
        if (ws.suspended) continue;
        if (ws.id === workspacesStore.activeWorkspaceId) continue;
        const lastActive = workspacesStore.lastSwitchedAt.get(ws.id);
        if (lastActive && lastActive < cutoff) {
          workspacesStore.suspendWorkspace(ws.id);
        }
      }
    }, 60_000); // check every minute

    return () => clearInterval(interval);
  });

  onMount(() => {
    workspacesStore.load().then(() => {
      loading = false;
      // Seed navigation history with the initial active tab
      const ws = workspacesStore.activeWorkspace;
      const pane = ws?.panes.find(p => p.id === ws.active_pane_id);
      if (ws && pane?.active_tab_id) {
        navHistoryStore.push({ workspaceId: ws.id, paneId: pane.id, tabId: pane.active_tab_id });
      }
      // Rebuild Agent Bridges from persisted state (after workspaces are loaded).
      agentBridgeStore.rehydrate();
      // Rebuild Mesh routers + topic registries from persisted state.
      import('$lib/stores/agentMesh.svelte').then(m => m.agentMeshStore.rehydrate()).catch(() => {});
    });

    // Listen for tab deactivation requests (e.g. "Suspend Other Tabs")
    function handleDeactivateTabs(e: Event) {
      const tabIds = (e as CustomEvent<string[]>).detail;
      for (const id of tabIds) activatedTabIds.delete(id);
    }
    window.addEventListener('deactivate-tabs', handleDeactivateTabs);

    // Wake a suspended tab (mesh setup "Wake all"): activate it so its TerminalPane mounts and
    // its auto-resume fires; clear any pending-resume gate on its pane so it doesn't sit waiting.
    function handleActivateTab(e: Event) {
      const tabId = (e as CustomEvent<string>).detail;
      if (!tabId) return;
      for (const ws of workspacesStore.workspaces) {
        for (const pane of ws.panes) {
          if (pane.tabs.some(t => t.id === tabId)) { pendingResumePanes.delete(pane.id); break; }
        }
      }
      activatedTabIds.add(tabId);
    }
    window.addEventListener('mesh-activate-tab', handleActivateTab);
    return () => {
      window.removeEventListener('deactivate-tabs', handleDeactivateTabs);
      window.removeEventListener('mesh-activate-tab', handleActivateTab);
    };
  });

  function handleSidebarResize(delta: number) {
    workspacesStore.setSidebarWidth(workspacesStore.sidebarWidth + delta);
  }

  function handleSidebarResizeEnd() {
    workspacesStore.saveSidebarWidth();
  }

  function handleTitlebarMouseDown(e: MouseEvent) {
    if (e.button === 0) getCurrentWindow().startDragging();
  }
</script>

<div class="app">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="titlebar" onmousedown={handleTitlebarMouseDown}>
    <span class="titlebar-text">
      {#if workspacesStore.activeWorkspace}{workspacesStore.activeWorkspace.name}{/if}
    </span>
    <div class="titlebar-logo" role="img" aria-label="maiTerm"></div>
  </div>
  <div class="app-body">
    {#if loading}
      <div class="loading">
        <div class="loading-logo" role="img" aria-label="maiTerm"></div>
      </div>
    {:else}
      <div
        class="sidebar-wrapper"
        class:collapsed={workspacesStore.sidebarCollapsed}
        style="width: {workspacesStore.sidebarCollapsed ? 0 : workspacesStore.sidebarWidth + 4}px"
      >
        <WorkspaceSidebar width={workspacesStore.sidebarWidth} onversionclick={() => showChangelog = true} onhelp={() => commands.openHelpWindow(workspacesStore.activeTab?.tab_type === 'editor' ? 'editor' : undefined)} />
        <Resizer direction="horizontal" onresize={handleSidebarResize} onresizeend={handleSidebarResizeEnd} />
      </div>
      {#if workspacesStore.sidebarCollapsed}
        <button class="sidebar-expand" onclick={() => workspacesStore.toggleSidebar()} title="Expand sidebar ({modSymbol}B)">
          <span class="expand-icon">&#x203A;</span>
        </button>
      {/if}

      <main class="main-content">
        {#if workspacesStore.activeWorkspace}
          {@const workspace = workspacesStore.activeWorkspace}
          {#if workspace.bridge_all && agentMeshStore.isStageView(workspace.id)}
            <!-- Mesh stage/filmstrip layout replaces the split tree for this workspace.
                 Terminals portal into its stage/filmstrip slots (data-terminal-slot). -->
            {#key workspace.id}
              <MeshStageView workspaceId={workspace.id} />
            {/key}
          {:else if workspace.split_root}
            {#key workspace.id}
              <SplitContainer
                node={workspace.split_root}
                workspaceId={workspace.id}
                panes={workspace.panes}
              />
            {/key}
          {/if}
        {:else}
          {@const suspendedWorkspaces = workspacesStore.workspaces.filter(w => w.suspended)}
          {@const activeWorkspaces = workspacesStore.workspaces.filter(w => !w.suspended)}
          <div class="empty-state">
            {#if suspendedWorkspaces.length > 0}
              {#if activeWorkspaces.length > 0}
                <p>This workspace is suspended</p>
                <p class="hint">Switch to an active workspace</p>
                <div class="suspended-list">
                  {#each activeWorkspaces as ws (ws.id)}
                    <button class="resume-btn" onclick={() => workspacesStore.setActiveWorkspace(ws.id)}>
                      {ws.name}
                    </button>
                  {/each}
                </div>
                <p class="hint" style="margin-top: 16px">Or resume a suspended workspace</p>
                <div class="suspended-list">
                  {#each suspendedWorkspaces as ws (ws.id)}
                    <button class="resume-btn suspended" onclick={() => workspacesStore.resumeWorkspace(ws.id)}>
                      {ws.name}
                    </button>
                  {/each}
                </div>
              {:else}
                <p>All workspaces suspended</p>
                <div class="suspended-list">
                  {#each suspendedWorkspaces as ws (ws.id)}
                    <button class="resume-btn" onclick={() => workspacesStore.resumeWorkspace(ws.id)}>
                      {ws.name}
                    </button>
                  {/each}
                </div>
              {/if}
              <p class="hint">Click to resume, or press <kbd>{modLabel}+N</kbd> to create a new workspace</p>
            {:else}
              <p>No workspace selected</p>
              <p>Press <kbd>{modLabel}+N</kbd> to create a new workspace</p>
            {/if}
          </div>
        {/if}

        <!-- Portal layer: terminals rendered flat across visited workspaces so they
             survive both split tree changes and workspace switches.
             Lazy: only mounts terminals once a workspace is first activated. -->
        <div class="terminal-host">
          {#each workspacesStore.workspaces.filter(w => activatedWorkspaceIds.has(w.id)) as ws (ws.id)}
            {@const meshStage = ws.id === workspacesStore.activeWorkspaceId && ws.bridge_all && agentMeshStore.isStageView(ws.id)}
            {#each ws.panes as pane (pane.id)}
              {#each pane.tabs as tab (tab.id)}
                {#if tab.tab_type === 'diff' && tab.diff_context}
                  <DiffPane
                    workspaceId={ws.id}
                    paneId={pane.id}
                    tabId={tab.id}
                    visible={!meshStage && tab.id === pane.active_tab_id && ws.id === workspacesStore.activeWorkspaceId}
                    diffContext={tab.diff_context}
                  />
                {:else if tab.tab_type === 'editor' && tab.editor_file}
                  <EditorPane
                    workspaceId={ws.id}
                    paneId={pane.id}
                    tabId={tab.id}
                    visible={!meshStage && tab.id === pane.active_tab_id && ws.id === workspacesStore.activeWorkspaceId}
                    editorFile={tab.editor_file}
                  />
                {:else if tab.tab_type === 'terminal' && (activatedTabIds.has(tab.id) || (meshStage && agentMeshStore.isMeshMemberTab(tab.id)))}
                  <TerminalPane
                    workspaceId={ws.id}
                    paneId={pane.id}
                    tabId={tab.id}
                    existingPtyId={terminalsStore.get(tab.id) ? tab.pty_id : null}
                    visible={meshStage ? agentMeshStore.isMeshMemberTab(tab.id) : (tab.id === pane.active_tab_id && ws.id === workspacesStore.activeWorkspaceId)}
                    restoreCwd={tab.restore_cwd}
                    restoreSshCommand={tab.restore_ssh_command}
                    restoreRemoteCwd={tab.restore_remote_cwd}
                    autoResumeCwd={tab.auto_resume_cwd}
                    autoResumeSshCommand={tab.auto_resume_ssh_command}
                    autoResumeRemoteCwd={tab.auto_resume_remote_cwd}
                    autoResumeCommand={tab.auto_resume_command}
                    autoResumeRememberedCommand={tab.auto_resume_remembered_command}
                    autoResumePinned={tab.auto_resume_pinned}
                    autoResumeEnabled={tab.auto_resume_enabled}
                    triggerVariables={tab.trigger_variables}
                  />
                {/if}
              {/each}
            {/each}
          {/each}
        </div>
      </main>
    {/if}
  </div>
</div>

<ChangelogModal open={showChangelog} onclose={() => showChangelog = false} version={appVersion} />

<style>
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .titlebar {
    position: relative;
    height: 36px;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
  }

  .titlebar-text {
    font-size: 0.923rem;
    color: var(--fg);
    pointer-events: none;
  }

  .titlebar-logo {
    position: absolute;
    right: 12px;
    top: 50%;
    transform: translateY(-50%);
    height: 16px;
    aspect-ratio: 2745 / 489;
    opacity: 0.8;
    pointer-events: none;
    background: var(--logo-url, url(/logo-light.png)) center / contain no-repeat;
  }

  .app-body {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  .loading {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .loading-logo {
    height: 48px;
    aspect-ratio: 2745 / 489;
    opacity: 0.5;
    background: var(--logo-url, url(/logo-light.png)) center / contain no-repeat;
  }

  .sidebar-wrapper {
    flex-shrink: 0;
    display: flex;
    overflow: hidden;
    transition: width 0.2s ease;
  }

  .sidebar-wrapper.collapsed {
    width: 0 !important;
  }

  .main-content {
    flex: 1;
    display: flex;
    min-width: 0;
    background: var(--bg-dark);
  }

  .empty-state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    color: var(--fg-dim);
  }

  .empty-state kbd {
    padding: 2px 6px;
    background: var(--bg-medium);
    border-radius: 4px;
    font-family: inherit;
  }

  .empty-state .hint {
    font-size: 0.85em;
    margin-top: 8px;
  }

  .suspended-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 8px;
    max-height: 300px;
    overflow-y: auto;
  }

  .resume-btn {
    padding: 8px 20px;
    background: var(--bg-medium);
    color: var(--fg);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
    font-size: 0.9em;
    transition: background 0.15s, border-color 0.15s;
  }

  .resume-btn:hover {
    background: var(--bg-light);
    border-color: var(--accent);
  }

  .resume-btn.suspended {
    opacity: 0.7;
  }

  .terminal-host {
    position: absolute;
    width: 0;
    height: 0;
    overflow: hidden;
    pointer-events: none;
  }

  .sidebar-expand {
    flex-shrink: 0;
    width: 20px;
    background: var(--bg-medium);
    border: none;
    border-right: 1px solid var(--bg-light);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    transition: background 0.1s;
  }

  .sidebar-expand:hover {
    background: var(--bg-light);
  }

  .expand-icon {
    color: var(--fg-dim);
    font-size: 1.231rem;
    line-height: 1;
  }

  .sidebar-expand:hover .expand-icon {
    color: var(--fg);
  }
</style>
