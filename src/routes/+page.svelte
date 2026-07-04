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
  import SessionRestoreModal from '$lib/components/SessionRestoreModal.svelte';
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

  // Sequential session-restore progress (drives SessionRestoreModal). On launch we
  // bring every tab that was live at last shutdown back one at a time — mounting
  // them all in a single reaction pinwheels the main thread (each xterm mount is a
  // layout + reflow). Restoring serially keeps the window responsive and lets the
  // user cancel.
  let restoreActive = $state(false);
  let restoreTotal = $state(0);
  let restoreDone = $state(0);
  let restoreCurrentLabel = $state('');
  let restoreCancelling = $state(false);
  // >0 while a restore/wake drive is pending or running. Starts at 1 (held for
  // the launch session restore) so startup work that must see the *settled* tab
  // set — e.g. the mesh auto-recheck — defers until respawns complete instead of
  // firing mid-restore against half-spawned members. A workspace resume raises
  // the gate again while its previously-live tabs are respawning.
  let restoreGate = $state(1);
  const restoreInProgress = $derived(restoreGate > 0);
  // Non-reactive guard read inside the async loop (no need to trigger effects).
  let restoreCancelled = false;
  // Only surface the modal for a meaningful restore — a 1-2 tab restore happens
  // silently (still serial) so normal launches don't flash a dialog.
  const RESTORE_MODAL_THRESHOLD = 3;

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
      // Full-session restore auto-resumes on visit instead of showing a manual
      // resume prompt on workspace switch.
      const fullRestore = preferencesStore.restoreSession && preferencesStore.sessionRestoreMode === 'all';
      // On workspace switch (not initial load), suspended tabs show a resume prompt.
      for (const paneSnap of paneSnapshots) {
        const tabId = paneSnap.active_tab_id;
        if (!tabId) continue;
        const tab = paneSnap.tabs.find(t => t.id === tabId);
        const isTerminal = tab && (tab.tab_type === 'terminal' || !tab.tab_type);
        // Only treat as suspended if the tab previously had a PTY (pty_id set but no live instance).
        // Brand-new tabs have pty_id === null and should activate immediately.
        const isSuspended = isTerminal && !!tab?.pty_id && !terminalsStore.get(tabId) && !activatedTabIds.has(tabId);

        if (initialActivationDone && workspaceSwitched && isSuspended && !fullRestore) {
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

  // When a mesh workspace becomes active (incl. on startup after an app restart), offer an
  // auto re-check if any of its agents dropped — agentMesh waits out auto-resume first.
  // Hold off while session restore is still respawning tabs: maybeAutoRecheck latches
  // (runs once per workspace) and its readiness probe would see the not-yet-restored
  // members as "dropped" and pop the mesh setup modal. Reading restoreInProgress here
  // makes the effect re-run when restore completes, firing the recheck against the
  // settled roster.
  $effect(() => {
    const id = workspacesStore.activeWorkspaceId;
    if (restoreInProgress) return;
    if (id) agentMeshStore.maybeAutoRecheck(id);
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

  interface RestoreItem {
    workspaceId: string;
    paneId: string;
    tabId: string;
    label: string;
  }

  // The ordered list of terminal tabs to bring back live. After the one-time boot
  // reconcile (Rust `reconcile_tab_liveness`), `pty_id` is authoritative — set ⟺
  // the tab had a live PTY at the last shutdown — and the suspend/close paths keep
  // it that way, so restore just respawns the pty_id-set tabs. Also covers a
  // still-running PTY on a window *reload* (reattach) and the visible workspace's
  // active tab (the activation effect mounts it regardless). Active workspace
  // first so the view the user is looking at comes back fastest; suspended
  // workspaces stay dormant.
  function buildRestoreList(): RestoreItem[] {
    const respawnLive = preferencesStore.restoreSession;
    const wss = workspacesStore.workspaces;
    const activeWsId = workspacesStore.activeWorkspaceId;
    const activeWs = wss.find(w => w.id === activeWsId);
    const orderedWs = activeWs
      ? [activeWs, ...wss.filter(w => w.id !== activeWsId)]
      : [...wss];

    const list: RestoreItem[] = [];
    for (const ws of orderedWs) {
      if (ws.suspended) continue;
      for (const pane of ws.panes) {
        const activeTab = pane.tabs.find(t => t.id === pane.active_tab_id);
        const orderedTabs = activeTab
          ? [activeTab, ...pane.tabs.filter(t => t.id !== pane.active_tab_id)]
          : [...pane.tabs];
        for (const tab of orderedTabs) {
          const isTerminal = tab.tab_type === 'terminal' || !tab.tab_type;
          if (!isTerminal) continue;
          const needsReattach = terminalsStore.shouldReattach(tab.pty_id);
          // pty_id set and not explicitly suspended ⟹ was live at last shutdown.
          const wasLive = respawnLive && !!tab.pty_id && !tab.suspended_at;
          // Live-at-suspend tab of a workspace load() just un-suspended (full
          // restore) — its pty_id was cleared at suspend, so the hint set is
          // the only marker.
          const pendingWake = workspacesStore.pendingWakeTabIds.has(tab.id);
          const isActiveVisible = ws.id === activeWsId && tab.id === pane.active_tab_id;
          if (!needsReattach && !wasLive && !pendingWake && !isActiveVisible) continue;
          list.push({
            workspaceId: ws.id,
            paneId: pane.id,
            tabId: tab.id,
            label: `${ws.name} › ${tab.name || 'Terminal'}`,
          });
        }
      }
    }
    return list;
  }

  // Resolve once the tab's TerminalPane has registered (PTY spawned/reattached),
  // or after a timeout so one wedged tab can't stall the queue. The trailing
  // breather lets that mount's layout/reflow finish before the next one starts.
  function waitForTabSettled(tabId: string, timeoutMs = 5000): Promise<void> {
    return new Promise((resolve) => {
      const start = Date.now();
      const tick = () => {
        if (terminalsStore.get(tabId) || Date.now() - start > timeoutMs) {
          setTimeout(resolve, 120);
          return;
        }
        setTimeout(tick, 50);
      };
      tick();
    });
  }

  // Drive a restore list serially, updating the progress modal as it goes.
  // Auto-resume commands fire inside each TerminalPane mount (we don't await
  // them — the queue only waits for the PTY to come up), so agents relaunch in
  // the background while the next tab restores. Shared by the launch session
  // restore and workspace resume (wake the tabs that were live at suspend).
  async function driveRestore(list: RestoreItem[]) {
    if (list.length === 0) return;

    restoreCancelled = false;
    restoreCancelling = false;
    restoreDone = 0;
    restoreTotal = list.length;
    restoreCurrentLabel = list[0].label;
    restoreActive = list.length >= RESTORE_MODAL_THRESHOLD;

    try {
      for (const item of list) {
        if (restoreCancelled) break;
        restoreCurrentLabel = item.label;
        try {
          // A woken tab shouldn't sit behind its pane's resume-on-click gate.
          pendingResumePanes.delete(item.paneId);
          activatedWorkspaceIds.add(item.workspaceId);
          activatedTabIds.add(item.tabId);
          await waitForTabSettled(item.tabId);
        } catch {
          // Never let one bad tab abort the whole restore.
        }
        restoreDone += 1;
      }
    } finally {
      restoreActive = false;
      restoreCancelling = false;
      restoreCurrentLabel = '';
    }
  }

  // Serialize drives — a workspace resume can land while the launch restore is
  // still draining; chaining keeps the mounts one-at-a-time globally.
  let restoreChain: Promise<void> = Promise.resolve();
  function queueRestore(list: RestoreItem[]): Promise<void> {
    restoreChain = restoreChain.then(() => driveRestore(list));
    return restoreChain;
  }

  async function runSessionRestore() {
    try {
      const list = buildRestoreList();
      workspacesStore.pendingWakeTabIds.clear();
      await queueRestore(list);
    } finally {
      // Restore is done (or there was nothing to do, or it was cancelled mid-drain).
      // Release the gate so deferred startup work — the mesh auto-recheck — runs now
      // that the tab set has settled.
      restoreGate -= 1;
    }
  }

  // Stop the restore. Dismiss the modal immediately for a snappy feel; the loop
  // drains its in-flight tab then breaks. Tabs already brought up stay live; the
  // rest keep their pty_id and fall back to the normal resume-on-click gate.
  function cancelSessionRestore() {
    restoreCancelled = true;
    restoreCancelling = true;
    restoreActive = false;
  }

  onMount(() => {
    workspacesStore.load().then(() => {
      loading = false;

      // Session restore: respawn the tabs that were live at last shutdown — by
      // then `pty_id` is the authoritative marker (the one-time boot reconcile
      // cleared the old high-watermark) — one at a time, with a progress modal.
      // The visible workspace's active tab is already activated by the $effect
      // above; runSessionRestore covers the rest and serializes the mounts so the
      // main thread stays responsive. The existingPtyId prop decides reattach
      // (live PTY, reload) vs respawn (restart) per tab at render time.
      // Fire-and-forget — nav history + bridge/mesh rehydrate below run now.
      runSessionRestore();

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

    // Workspace resume: bring back the tabs that were live when it was suspended,
    // serially like session restore. The gate is raised synchronously — the store
    // dispatches this before setActiveWorkspace — so the mesh auto-recheck effect
    // fired by the imminent workspace switch waits for the settled roster.
    function handleWorkspaceResumeTabs(e: Event) {
      const items = (e as CustomEvent<RestoreItem[]>).detail;
      if (!items?.length) return;
      restoreGate += 1;
      queueRestore(items).finally(() => { restoreGate -= 1; });
    }
    window.addEventListener('workspace-resume-tabs', handleWorkspaceResumeTabs);

    return () => {
      window.removeEventListener('deactivate-tabs', handleDeactivateTabs);
      window.removeEventListener('mesh-activate-tab', handleActivateTab);
      window.removeEventListener('workspace-resume-tabs', handleWorkspaceResumeTabs);
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
                    existingPtyId={(terminalsStore.get(tab.id) || terminalsStore.shouldReattach(tab.pty_id)) ? tab.pty_id : null}
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

{#if restoreActive}
  <SessionRestoreModal
    total={restoreTotal}
    done={restoreDone}
    currentLabel={restoreCurrentLabel}
    cancelling={restoreCancelling}
    oncancel={cancelSessionRestore}
  />
{/if}

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
