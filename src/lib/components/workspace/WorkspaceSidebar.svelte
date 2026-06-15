<script lang="ts">
  import { tick } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { activityStore } from '$lib/stores/activity.svelte';
  import { claudeStateStore, type WorkspaceClaudeState } from '$lib/stores/agentState.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import * as commands from '$lib/tauri/commands';
  import { modSymbol } from '$lib/utils/platform';
  import { claudeCodeStore } from '$lib/stores/claudeCode.svelte';
  import { navHistoryStore } from '$lib/stores/navHistory.svelte';
  import { openPreferencesWindow } from '$lib/tauri/commands';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { untrack } from 'svelte';
  import { updaterStore } from '$lib/stores/updater.svelte';
  import ChangelogModal from '$lib/components/ChangelogModal.svelte';
  import type { ChangelogEntry } from '$lib/components/ChangelogModal.svelte';
  import type { Update } from '@tauri-apps/plugin-updater';

  let showWhatsNew = $state(false);
  let whatsNewEntries = $state<ChangelogEntry[]>([]);
  let newerVersionPrompt = $state<{ version: string; originalVersion: string } | undefined>(undefined);
  let newerUpdate = $state<Update | null>(null);
  let rechecking = $state(false);

  async function openWhatsNew() {
    const entries = await updaterStore.fetchReleaseNotes();
    whatsNewEntries = entries;
    showWhatsNew = true;
  }

  // Watch for toast-triggered "show what's new" requests
  $effect(() => {
    if (updaterStore.showWhatsNewRequested) {
      untrack(() => {
        updaterStore.clearShowWhatsNewRequest();
        openWhatsNew();
      });
    }
  });

  async function handleInstallFromModal() {
    if (rechecking) return;
    rechecking = true;
    try {
      const newer = await updaterStore.recheckForNewer();
      if (newer) {
        newerUpdate = newer;
        newerVersionPrompt = {
          version: newer.version,
          originalVersion: updaterStore.currentUpdate!.version,
        };
        return;
      }
    } finally {
      rechecking = false;
    }
    await proceedWithInstall();
  }

  async function handleInstallLatest() {
    if (newerUpdate) {
      updaterStore.switchToUpdate(newerUpdate);
    }
    newerVersionPrompt = undefined;
    newerUpdate = null;
    await proceedWithInstall();
  }

  async function handleInstallOriginal() {
    newerVersionPrompt = undefined;
    newerUpdate = null;
    await proceedWithInstall();
  }

  async function handleReviewLatest() {
    if (newerUpdate) {
      updaterStore.switchToUpdate(newerUpdate);
    }
    newerVersionPrompt = undefined;
    newerUpdate = null;
    const entries = await updaterStore.fetchReleaseNotes();
    whatsNewEntries = entries;
  }

  async function proceedWithInstall() {
    await updaterStore.downloadAndInstall();
    if (updaterStore.installed) {
      updaterStore.restart();
    }
  }

  // Always-visible footer agent indicators. Up to three dots — working (blue),
  // waiting-for-permission (red), finished-unread (green) — so "does anything
  // need me?" is glanceable even with the sidebar collapsed or workspaces below
  // the fold. Each dot is independent: clicking it jumps to a tab in that state,
  // and repeated clicks cycle through every agent in that state (see cycleToAgent).
  // Every tab ID owned by this window. Claude hook events broadcast to all
  // windows, so the global session map includes agents from other windows; scope
  // the footer dot to this window's tabs so each window's dot is independent and a
  // click can always navigate to its target (navigateToTab only searches here).
  const windowTabIds = $derived.by(() => {
    const ids = new Set<string>();
    for (const ws of workspacesStore.workspaces)
      for (const pane of ws.panes)
        for (const t of pane.tabs) ids.add(t.id);
    return ids;
  });

  type FooterDot = {
    color: 'accent' | 'green' | 'red' | 'dim';
    pulse: boolean;
    hollow: boolean;
    tooltip: string;
    targets: string[];
  };

  // Three fixed slots — left | center | right — each holding a dot or nothing.
  // Home slots: working=left, waiting=center, finished=right. A lone signal sits
  // in the center slot (a single dot reads better centered than shoved to a
  // side); two or more keep their home slots, so "waiting" always anchors the
  // middle and the others flank it. When nothing is signaling, a single resting
  // dot occupies the center (hollow green = all done & seen, dim = no agents).
  const footerDots = $derived.by((): (FooterDot | null)[] => {
    const b = claudeStateStore.getClaudeStateBreakdown(windowTabIds);
    const cycle = (c: number) => (c > 1 ? ' (click to cycle)' : '');

    const working: FooterDot | null = b.active.length
      ? { color: 'accent', pulse: true, hollow: false, targets: b.active,
          tooltip: `${b.active.length === 1 ? '1 agent' : `${b.active.length} agents`} working — click to view${cycle(b.active.length)}` }
      : null;
    const waiting: FooterDot | null = b.permission.length
      ? { color: 'red', pulse: true, hollow: false, targets: b.permission,
          tooltip: `${b.permission.length === 1 ? '1 agent needs' : `${b.permission.length} agents need`} permission — click to open${cycle(b.permission.length)}` }
      : null;
    const finished: FooterDot | null = b.idleUnread.length
      ? { color: 'green', pulse: false, hollow: false, targets: b.idleUnread,
          tooltip: `${b.idleUnread.length === 1 ? '1 agent' : `${b.idleUnread.length} agents`} finished — click to review${cycle(b.idleUnread.length)}` }
      : null;

    const present = [working, waiting, finished].filter((d): d is FooterDot => d !== null);
    if (present.length === 0) {
      const resting: FooterDot = b.idleRead.length
        ? { color: 'green', pulse: false, hollow: true, targets: b.idleRead, tooltip: 'All agents idle' }
        : { color: 'dim', pulse: false, hollow: false, targets: [], tooltip: 'No active agents' };
      return [null, resting, null];
    }
    if (present.length === 1) return [null, present[0], null];
    return [working, waiting, finished];
  });

  // Cycle one footer dot through every agent in its state. Anchored on the
  // currently-viewed tab: if it's one of the targets, advance to the next
  // (wrapping); otherwise jump to the first. Stateless, so it self-corrects when
  // the target list shifts as agents change state.
  function cycleToAgent(targets: string[]) {
    if (targets.length === 0) return;
    const currentIdx = targets.indexOf(workspacesStore.activeTab?.id ?? '');
    const next = currentIdx === -1 ? targets[0] : targets[(currentIdx + 1) % targets.length];
    navigateToTab(next);
  }

  function workspaceHasActivity(workspaceId: string): boolean {
    if (workspaceId === workspacesStore.activeWorkspaceId) return false;
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (!ws) return false;
    const tabIds = ws.panes.flatMap(p => p.tabs.map(t => t.id));
    return activityStore.hasAnyActivity(tabIds);
  }

  function workspaceTabState(workspaceId: string): 'alert' | 'question' | null {
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (!ws) return null;
    const tabIds = ws.panes.flatMap(p => p.tabs.map(t => t.id));
    return activityStore.getWorkspaceTabState(tabIds);
  }

  function workspaceClaudeState(workspaceId: string): WorkspaceClaudeState | null {
    if (workspaceId === workspacesStore.activeWorkspaceId) return null;
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (!ws) return null;
    const tabIds = ws.panes.flatMap(p => p.tabs.map(t => t.id));
    return claudeStateStore.getWorkspaceClaudeState(tabIds);
  }

  let appVersion = $state('');
  getVersion().then(v => { appVersion = v; });

  interface Props {
    width: number;
    onversionclick?: () => void;
    onhelp?: () => void;
  }

  let { width, onversionclick, onhelp }: Props = $props();

  let editingId = $state<string | null>(null);
  let editingName = $state('');
  let editInput = $state<HTMLInputElement | null>(null);

  async function startEditing(id: string, currentName: string) {
    editingId = id;
    editingName = currentName;
    await tick();
    editInput?.select();
  }

  async function finishEditing() {
    if (editingId && editingName.trim()) {
      await workspacesStore.renameWorkspace(editingId, editingName.trim());
    }
    editingId = null;
    editingName = '';
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      finishEditing();
    } else if (e.key === 'Escape') {
      editingId = null;
      editingName = '';
    }
  }

  async function handleNewWorkspace() {
    const count = workspacesStore.workspaces.length + 1;
    await workspacesStore.createWorkspace(`Workspace ${count}`);
  }

  let confirmingDeleteId = $state<string | null>(null);

  function handleDeleteWorkspace(id: string, e: MouseEvent) {
    e.stopPropagation();
    confirmingDeleteId = id;
  }

  async function doDeleteWorkspace(id: string) {
    confirmingDeleteId = null;
    if (workspacesStore.workspaces.length > 1) {
      await workspacesStore.deleteWorkspace(id);
    } else {
      // Last workspace: kill terminals and show empty state
      await terminalsStore.killAllTerminals();
      await commands.resetWindow();
      workspacesStore.reset();
    }
  }

  async function handleSuspendWorkspace(id: string, e: MouseEvent) {
    e.stopPropagation();
    await workspacesStore.suspendWorkspace(id);
  }

  async function handleSuspendAllOthers(e: MouseEvent) {
    e.stopPropagation();
    await workspacesStore.suspendAllOtherWorkspaces();
  }

  // Pointer-based drag reordering (same pattern as TerminalTabs)
  let dragWorkspaceId = $state<string | null>(null);
  let dropTargetIndex = $state<number | null>(null);
  let dropSide = $state<'before' | 'after'>('before');

  const DRAG_THRESHOLD = 5;
  let dragStartX = 0;
  let dragStartY = 0;
  let lastPointerX = 0;
  let lastPointerY = 0;
  let pendingDragWorkspaceId: string | null = null;
  let ghost: HTMLElement | null = null;
  let cursorBadge: HTMLElement | null = null;
  let workspaceListEl: HTMLElement;
  let didDrag = false;

  function handlePointerDown(e: PointerEvent, workspaceId: string) {
    if (e.button !== 0 || editingId === workspaceId) return;
    if ((e.target as HTMLElement).closest('.tooltip-wrapper, .confirm-delete, .confirm-cancel')) return;
    pendingDragWorkspaceId = workspaceId;
    dragStartX = e.clientX;
    dragStartY = e.clientY;
    didDrag = false;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function handlePointerMove(e: PointerEvent) {
    if (!pendingDragWorkspaceId && !dragWorkspaceId) return;

    // Check threshold before starting drag
    if (pendingDragWorkspaceId && !dragWorkspaceId) {
      const dx = e.clientX - dragStartX;
      const dy = e.clientY - dragStartY;
      if (Math.abs(dx) < DRAG_THRESHOLD && Math.abs(dy) < DRAG_THRESHOLD) return;
      dragWorkspaceId = pendingDragWorkspaceId;
      pendingDragWorkspaceId = null;
      didDrag = true;
      createGhost(e);
    }

    if (!dragWorkspaceId || !ghost) return;

    // Move ghost
    ghost.style.left = `${e.clientX}px`;
    ghost.style.top = `${e.clientY}px`;

    // Hit-test workspace items to find drop target (vertical)
    const wsEls = workspaceListEl.querySelectorAll<HTMLElement>('.workspace-item');
    let foundTarget = false;
    for (let i = 0; i < wsEls.length; i++) {
      const rect = wsEls[i].getBoundingClientRect();
      if (e.clientX >= rect.left && e.clientX <= rect.right &&
          e.clientY >= rect.top && e.clientY <= rect.bottom) {
        const midY = rect.top + rect.height / 2;
        dropSide = e.clientY < midY ? 'before' : 'after';
        dropTargetIndex = i;
        foundTarget = true;
        break;
      }
    }
    // If cursor is below the last item but within the list, target "after last"
    if (!foundTarget && wsEls.length > 0) {
      const listRect = workspaceListEl.getBoundingClientRect();
      const lastRect = wsEls[wsEls.length - 1].getBoundingClientRect();
      if (e.clientX >= listRect.left && e.clientX <= listRect.right &&
          e.clientY > lastRect.bottom && e.clientY <= listRect.bottom) {
        dropTargetIndex = wsEls.length - 1;
        dropSide = 'after';
        foundTarget = true;
      }
    }
    if (!foundTarget) {
      dropTargetIndex = null;
    }

    lastPointerX = e.clientX;
    lastPointerY = e.clientY;
    updateCursorBadge(e.altKey);
  }

  function updateCursorBadge(altKey: boolean) {
    if (!cursorBadge) return;
    cursorBadge.style.left = `${lastPointerX + 16}px`;
    cursorBadge.style.top = `${lastPointerY + 16}px`;
    cursorBadge.style.display = altKey ? 'flex' : 'none';
  }

  function handleDragKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      clearDragState();
      return;
    }
    if (e.key === 'Alt') updateCursorBadge(true);
  }

  function handleDragKeyUp(e: KeyboardEvent) {
    if (e.key === 'Alt') updateCursorBadge(false);
  }

  function handlePointerUp(e: PointerEvent) {
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);

    if (dragWorkspaceId && dropTargetIndex !== null) {
      const sourceId = dragWorkspaceId;
      const allWs = workspacesStore.workspaces;
      const fromIndex = allWs.findIndex(w => w.id === sourceId);
      const isCopy = e.altKey;

      // Compute the insertion position
      let insertPos = dropSide === 'after' ? dropTargetIndex + 1 : dropTargetIndex;

      clearDragState();

      if (isCopy) {
        // Duplicate workspace at the insertion position
        workspacesStore.duplicateWorkspace(sourceId, insertPos);
      } else if (fromIndex !== -1) {
        // Reorder: compute new order
        let toIndex = insertPos;
        if (fromIndex < toIndex) toIndex--;
        if (fromIndex !== toIndex) {
          const ids = allWs.map(w => w.id);
          const [moved] = ids.splice(fromIndex, 1);
          ids.splice(toIndex, 0, moved);
          workspacesStore.reorderWorkspaces(ids);
        }
      }
      return;
    }

    clearDragState();
  }

  function createGhost(e: PointerEvent) {
    const sourceEl = workspaceListEl.querySelector<HTMLElement>(
      `.workspace-item[data-workspace-id="${dragWorkspaceId}"]`
    );
    if (!sourceEl) return;
    ghost = sourceEl.cloneNode(true) as HTMLElement;
    ghost.classList.add('drag-ghost');
    ghost.style.width = `${sourceEl.offsetWidth}px`;
    ghost.style.left = `${e.clientX}px`;
    ghost.style.top = `${e.clientY}px`;
    document.body.appendChild(ghost);
    // Cursor badge (macOS-style "+" near pointer)
    cursorBadge = document.createElement('div');
    cursorBadge.className = 'drag-cursor-badge';
    cursorBadge.textContent = '+';
    cursorBadge.style.display = 'none';
    document.body.appendChild(cursorBadge);
    // Key listeners for Escape cancel and Option badge
    document.addEventListener('keydown', handleDragKeyDown);
    document.addEventListener('keyup', handleDragKeyUp);
  }

  function clearDragState() {
    document.removeEventListener('keydown', handleDragKeyDown);
    document.removeEventListener('keyup', handleDragKeyUp);
    dragWorkspaceId = null;
    dropTargetIndex = null;
    pendingDragWorkspaceId = null;
    if (ghost) {
      ghost.remove();
      ghost = null;
    }
    if (cursorBadge) {
      cursorBadge.remove();
      cursorBadge = null;
    }
  }

  const sortedWorkspaces = $derived.by(() => {
    const ws = workspacesStore.workspaces;
    const order = preferencesStore.workspaceSortOrder;
    if (order === 'alphabetical') {
      return [...ws].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }));
    }
    if (order === 'recent_activity') {
      const switched = workspacesStore.lastSwitchedAt;
      return [...ws].sort((a, b) => {
        const aTs = a.id === workspacesStore.activeWorkspaceId ? Date.now() : (switched.get(a.id) ?? 0);
        const bTs = b.id === workspacesStore.activeWorkspaceId ? Date.now() : (switched.get(b.id) ?? 0);
        return bTs - aTs;
      });
    }
    return ws;
  });

  async function handleItemClick(workspaceId: string) {
    // Suppress click after a drag
    if (didDrag) {
      didDrag = false;
      return;
    }
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (ws?.suspended) {
      await workspacesStore.resumeWorkspace(workspaceId);
    } else {
      await workspacesStore.setActiveWorkspace(workspaceId);
    }
    // Push the target tab only if it has a live terminal (was activated this session)
    const targetWs = workspacesStore.workspaces.find(w => w.id === workspaceId);
    const targetPane = targetWs?.panes.find(p => p.id === targetWs?.active_pane_id);
    if (targetWs && targetPane?.active_tab_id) {
      const tab = targetPane.tabs.find(t => t.id === targetPane.active_tab_id);
      const isTerminal = tab && (tab.tab_type === 'terminal' || !tab.tab_type);
      if (!isTerminal || terminalsStore.get(targetPane.active_tab_id)) {
        navHistoryStore.push({ workspaceId: targetWs.id, paneId: targetPane.id, tabId: targetPane.active_tab_id });
      }
    }
  }
</script>

<aside class="sidebar" style="width: {width}px">
  <div class="sidebar-titlebar">
    <div class="sidebar-logo" role="img" aria-label="maiTerm"></div>
    {#if import.meta.env.DEV}
      <span class="dev-badge">DEV</span>
    {/if}
    {#if appVersion}
      <button class="version-badge" onclick={onversionclick}>v{appVersion}</button>
    {/if}
    {#if claudeCodeStore.connected}
      <span class="claude-connected">
        <StatusDot color="green" tooltip="IDE Connected" />
      </span>
    {/if}
    <span style="margin-left:auto"><IconButton tooltip="Collapse sidebar ({modSymbol}B)" size={20} style="font-size: 1.231rem" onclick={() => workspacesStore.toggleSidebar()}>&#x2039;</IconButton></span>
  </div>
  <div class="sidebar-header">
    <span class="title">WORKSPACES</span>
    <IconButton tooltip="Suspend all other workspaces" size={20} style="font-size: 0.769rem" onclick={handleSuspendAllOthers}><Icon name="pause" size={10} /></IconButton>
    <IconButton tooltip="New workspace ({modSymbol}N)" size={20} style="font-size: 1.231rem" onclick={handleNewWorkspace}>+</IconButton>
  </div>

  {#if preferencesStore.showRecentWorkspaces && workspacesStore.recentWorkspaces.length > 0}
    <div class="recent-section">
      <span class="recent-title">RECENT</span>
      <div class="recent-list">
        {#each workspacesStore.recentWorkspaces as workspace (workspace.id)}
          <button
            class="recent-item"
            onclick={() => handleItemClick(workspace.id)}
            title={workspace.name}
          >
            {workspace.name}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  <div class="workspace-list" bind:this={workspaceListEl}>
    {#each sortedWorkspaces as workspace, index (workspace.id)}
      <div
        class="workspace-item"
        class:active={workspace.id === workspacesStore.activeWorkspaceId}
        class:suspended={workspace.suspended}
        class:import-highlight={workspace.import_highlight}
        class:dragging={dragWorkspaceId === workspace.id}
        class:drop-before={dropTargetIndex === index && dropSide === 'before' && dragWorkspaceId !== workspace.id}
        class:drop-after={dropTargetIndex === index && dropSide === 'after' && dragWorkspaceId !== workspace.id}
        data-workspace-id={workspace.id}
        onclick={() => handleItemClick(workspace.id)}
        ondblclick={() => { if (!confirmingDeleteId) startEditing(workspace.id, workspace.name); }}
        onpointerdown={(e) => { if (!confirmingDeleteId) handlePointerDown(e, workspace.id); }}
        onpointermove={handlePointerMove}
        onpointerup={handlePointerUp}
        role="button"
        tabindex="0"
        onkeydown={(e) => e.key === 'Enter' && handleItemClick(workspace.id)}
      >
        {#if editingId === workspace.id}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            type="text"
            bind:value={editingName}
            bind:this={editInput}
            onblur={finishEditing}
            onkeydown={handleKeydown}
            class="edit-input"
            autofocus
          />
        {:else}
          {@const wsTabState = workspaceTabState(workspace.id)}
          {@const wsClaude = workspaceClaudeState(workspace.id)}
          {@const wsActivity = workspaceHasActivity(workspace.id)}
          {#if preferencesStore.showWorkspaceTabCount}
            <span class="tab-count-badge" class:active={workspace.id === workspacesStore.activeWorkspaceId} class:status-alert={wsTabState === 'alert'} class:status-question={wsTabState === 'question'} class:status-claude-active={!wsTabState && wsClaude === 'active'} class:status-claude-idle={!wsTabState && wsClaude === 'idle-unread'} class:status-claude-idle-read={!wsTabState && wsClaude === 'idle-read'} class:status-activity={!wsTabState && !wsClaude && wsActivity}>{workspace.panes.reduce((sum, p) => sum + p.tabs.length, 0)}</span>
          {:else}
            <span class="workspace-indicator">
              {#if wsTabState === 'alert'}
                <span class="state-emoji">&#x2757;</span>
              {:else if wsTabState === 'question'}
                <span class="state-emoji">&#x2753;</span>
              {:else if wsClaude === 'active'}
                <StatusDot color="accent" pulse tooltip="Claude is working" />
              {:else if wsClaude === 'idle-unread'}
                <StatusDot color="green" tooltip="Claude finished" />
              {:else if wsClaude === 'idle-read'}
                <StatusDot color="green" hollow tooltip="Claude finished (seen)" />
              {:else if workspace.id === workspacesStore.activeWorkspaceId}
                >
              {:else if wsActivity}
                <StatusDot color="dim" />
              {/if}
            </span>
          {/if}
          {#if confirmingDeleteId === workspace.id}
            <button class="confirm-delete" onclick={(e) => { e.stopPropagation(); doDeleteWorkspace(workspace.id); }}>Delete?</button>
            <button class="confirm-cancel" onclick={(e) => { e.stopPropagation(); confirmingDeleteId = null; }}>Cancel</button>
          {:else}
            <span class="workspace-name">{workspace.name}</span>
            {#if workspace.suspended}
              <IconButton
                tooltip="Delete workspace"
                class="workspace-close-btn"
                style="--icon-btn-hover: var(--bg-dark)"
                onclick={(e) => handleDeleteWorkspace(workspace.id, e)}
              >
                &times;
              </IconButton>
            {:else}
              <IconButton
                tooltip="Suspend workspace"
                class="workspace-close-btn"
                style="--icon-btn-hover: var(--bg-dark)"
                onclick={(e) => handleSuspendWorkspace(workspace.id, e)}
              >
                <Icon name="pause" size={10} />
              </IconButton>
            {/if}
          {/if}
        {/if}
      </div>
    {/each}
  </div>

  {#if updaterStore.showBanner}
    <div class="update-banner">
      <button class="update-dismiss" onclick={() => updaterStore.dismiss()} aria-label="Dismiss">&times;</button>
      {#if updaterStore.installed}
        <div class="update-text">Update installed</div>
        <button class="update-action" onclick={() => updaterStore.restart()}>Restart</button>
      {:else if updaterStore.downloading}
        <div class="update-text">Downloading v{updaterStore.currentUpdate?.version}…</div>
      {:else}
        <div class="update-text">
          v{updaterStore.currentUpdate?.version} available
          <button class="update-link" onclick={openWhatsNew}>What's new</button>
        </div>
        <button class="update-action" onclick={() => updaterStore.downloadAndInstall()}>Install</button>
      {/if}
    </div>
  {/if}

  <div class="sidebar-footer">
    <div class="footer-side">
      <IconButton tooltip="Report Bug" size={24} style="border-radius:4px" onclick={() => shellOpen('https://github.com/Flexmark-Intl/maiterm/issues/new?labels=bug&type=bug')}><Icon name="bug" size={14} /></IconButton>
      <IconButton tooltip="Feature Request" size={24} style="border-radius:4px" onclick={() => shellOpen('https://github.com/Flexmark-Intl/maiterm/issues/new?type=feature')}><Icon name="lightbulb" size={14} /></IconButton>
    </div>
    <span class="footer-spacer"></span>
    <div class="footer-agent-cluster">
      {#each footerDots as dot, i (i)}
        <div class="footer-agent-slot">
          {#if dot}
            <Tooltip text={dot.tooltip}>
              <button
                class="footer-agent-dot"
                class:clickable={dot.targets.length > 0}
                onclick={() => cycleToAgent(dot.targets)}
              >
                <StatusDot color={dot.color} pulse={dot.pulse} hollow={dot.hollow} />
              </button>
            </Tooltip>
          {/if}
        </div>
      {/each}
    </div>
    <span class="footer-spacer"></span>
    <div class="footer-side">
      <IconButton tooltip="Preferences ({modSymbol},)" size={24} style="border-radius:4px" onclick={openPreferencesWindow}><Icon name="settings" size={14} /></IconButton>
      <IconButton tooltip="Help ({modSymbol}/)" size={24} style="border-radius:4px" onclick={onhelp}><Icon name="help" size={14} /></IconButton>
    </div>
  </div>
</aside>

<ChangelogModal
  open={showWhatsNew}
  onclose={() => { showWhatsNew = false; newerVersionPrompt = undefined; newerUpdate = null; }}
  version={appVersion}
  entries={whatsNewEntries}
  title="What's New"
  oninstall={updaterStore.currentUpdate && !updaterStore.installed ? handleInstallFromModal : undefined}
  installLabel={rechecking ? 'Checking…' : updaterStore.downloading ? 'Downloading…' : updaterStore.installed ? 'Restarting…' : 'Install & Restart'}
  installDisabled={rechecking || updaterStore.downloading || updaterStore.installed}
  {newerVersionPrompt}
  oninstallLatest={handleInstallLatest}
  oninstallOriginal={handleInstallOriginal}
  onreviewLatest={handleReviewLatest}
/>

<style>
  .sidebar {
    flex-shrink: 0;
    background: var(--bg-medium);
    display: flex;
    flex-direction: column;
  }

  .sidebar-titlebar {
    display: flex;
    align-items: center;
    height: var(--tab-height);
    padding: 0 16px;
    border-bottom: 1px solid var(--bg-light);
  }

  .sidebar-logo {
    height: 20px;
    aspect-ratio: 3700 / 2717;
    opacity: 0.7;
    pointer-events: none;
    background: var(--logo-mark-url, url(/logo-mark-light.png)) center / contain no-repeat;
  }

  .dev-badge {
    margin-left: 6px;
    font-size: 0.769rem;
    font-weight: 600;
    color: var(--bg-dark);
    background: var(--accent);
    padding: 1px 6px;
    border-radius: 3px;
    letter-spacing: 0.5px;
    pointer-events: none;
  }

  .version-badge {
    margin-left: 6px;
    font-size: 0.769rem;
    color: var(--fg-dim);
    cursor: pointer;
    -webkit-app-region: no-drag;
  }

  .version-badge:hover {
    color: var(--fg);
  }

  .claude-connected {
    display: inline-flex;
    align-items: center;
    margin-left: 6px;
  }


  .sidebar-header {
    padding: 12px 16px;
    border-bottom: 1px solid var(--bg-light);
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .title {
    font-size: 0.846rem;
    font-weight: 600;
    letter-spacing: 0.5px;
    color: var(--fg-dim);
  }


  .recent-section {
    padding: 8px 16px;
    border-bottom: 1px solid var(--bg-light);
  }

  .recent-title {
    font-size: 0.769rem;
    font-weight: 600;
    letter-spacing: 0.5px;
    color: var(--fg-dim);
  }

  .recent-list {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 6px;
  }

  .recent-item {
    font-size: 0.846rem;
    padding: 2px 8px;
    border-radius: 3px;
    background: var(--bg-light);
    color: var(--fg);
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
    transition: background 0.1s;
  }

  .recent-item:hover {
    background: var(--accent);
    color: var(--bg-dark);
  }

  .workspace-list {
    flex: 1;
    overflow-y: auto;
    padding: 8px 0;
  }

  .workspace-item {
    display: flex;
    align-items: center;
    padding: 8px 16px;
    cursor: pointer;
    transition: background 0.1s;
    gap: 8px;
  }

  .workspace-item:hover {
    background: var(--bg-light);
  }

  .workspace-item.active {
    background: var(--bg-light);
  }

  .workspace-item:global(.drop-target) {
    background: rgba(122, 162, 247, 0.2);
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }

  .workspace-item.import-highlight {
    box-shadow: inset 3px 0 0 var(--yellow, #e0af68);
  }

  .workspace-item.suspended .workspace-name {
    color: var(--fg-dim);
  }

  .workspace-item.dragging {
    opacity: 0.3;
  }

  .workspace-item.drop-before {
    box-shadow: inset 0 2px 0 var(--accent);
  }

  .workspace-item.drop-after {
    box-shadow: inset 0 -2px 0 var(--accent);
  }

  .workspace-indicator {
    color: var(--accent);
    font-weight: bold;
    width: 12px;
    display: flex;
    align-items: center;
    justify-content: center;
  }


  .state-emoji {
    font-size: 0.769rem;
    line-height: 1;
  }

  .workspace-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tab-count-badge {
    flex-shrink: 0;
    font-size: 0.769rem;
    font-weight: 600;
    line-height: 1;
    min-width: 16px;
    padding: 1px 4px;
    border-radius: 3px;
    background: var(--bg-light);
    color: var(--fg);
    text-align: center;
    letter-spacing: 0.3px;
    border: 1px solid transparent;
  }

  .tab-count-badge.active {
    background: var(--accent);
    color: var(--bg-dark);
  }

  .tab-count-badge.status-alert {
    border-color: var(--red);
  }

  .tab-count-badge.status-question {
    border-color: var(--yellow);
  }

  .tab-count-badge.status-claude-active {
    border-color: var(--accent);
  }

  .tab-count-badge.status-claude-idle {
    border-color: var(--green);
  }

  .tab-count-badge.status-claude-idle-read {
    border-color: color-mix(in srgb, var(--green) 45%, transparent);
  }

  .tab-count-badge.status-activity {
    border-color: var(--fg-dim);
  }

  .confirm-delete, .confirm-cancel {
    font-size: 0.846rem;
    padding: 2px 8px;
    border: none;
    border-radius: 3px;
    cursor: pointer;
    transition: background 0.1s;
    -webkit-app-region: no-drag;
  }

  .confirm-delete {
    color: #f7768e;
    background: color-mix(in srgb, #f7768e 15%, transparent);
  }

  .confirm-delete:hover {
    background: color-mix(in srgb, #f7768e 30%, transparent);
  }

  .confirm-cancel {
    color: var(--fg);
    background: var(--bg-dark);
  }

  .confirm-cancel:hover {
    background: var(--bg-medium);
  }

  .workspace-item :global(.workspace-close-btn) {
    opacity: 0;
    flex-shrink: 0;
  }

  .workspace-item:hover :global(.workspace-close-btn),
  .workspace-item.active :global(.workspace-close-btn) {
    opacity: 1;
  }

  .edit-input {
    flex: 1;
    background: var(--bg-dark);
  }

  .update-banner {
    border-top: 1px solid var(--bg-light);
    padding: 8px 12px;
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--bg-dark);
    position: relative;
  }

  .update-text {
    font-size: 0.769rem;
    color: var(--fg);
    flex: 1;
    min-width: 0;
  }

  .update-action {
    font-size: 0.769rem;
    font-weight: 600;
    padding: 3px 10px;
    border: none;
    border-radius: 3px;
    background: var(--accent);
    color: var(--bg-dark);
    cursor: pointer;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .update-action:hover {
    filter: brightness(1.15);
  }

  .update-link {
    font-size: 0.692rem;
    color: var(--accent);
    background: none;
    border: none;
    cursor: pointer;
    padding: 0;
    text-decoration: underline;
    display: block;
    margin-top: 2px;
  }

  .update-link:hover {
    filter: brightness(1.2);
  }

  .update-dismiss {
    font-size: 1rem;
    line-height: 1;
    color: var(--fg-dim);
    background: none;
    border: none;
    cursor: pointer;
    padding: 0;
    flex-shrink: 0;
  }

  .update-dismiss:hover {
    color: var(--fg);
  }

  .sidebar-footer {
    border-top: 1px solid var(--bg-light);
    padding: 6px 8px;
    display: flex;
    align-items: center;
  }

  /* Bug/Feature on the left, Preferences/Help on the right. The two groups are
     equal width, so the centered agent cluster's middle slot lands at the
     footer's horizontal center. */
  .footer-side {
    display: flex;
    gap: 4px;
  }

  .footer-spacer {
    flex: 1;
  }

  /* Three fixed slots so the dots hold their left | center | right positions
     even when a slot is empty — the "waiting" dot stays anchored at the center. */
  .footer-agent-cluster {
    display: flex;
    align-items: center;
    flex-shrink: 0;
  }

  .footer-agent-slot {
    width: 16px;
    height: 24px;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  /* Each occupied slot is a bare dot with a comfortable click target. */
  .footer-agent-dot {
    display: flex;
    align-items: center;
    justify-content: center;
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    background: none;
    border: none;
    margin: 0;
    padding: 0;
    border-radius: 4px;
    cursor: default;
  }
  .footer-agent-dot.clickable {
    cursor: pointer;
  }
  .footer-agent-dot.clickable:hover {
    background: var(--bg-light);
  }

</style>
