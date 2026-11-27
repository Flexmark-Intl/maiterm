<script lang="ts">
  import { tick } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { workspaceIsLive } from '$lib/workspace/liveness';
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
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import '$lib/overlord/deck.css';
  import ChangelogModal from '$lib/components/ChangelogModal.svelte';
  import ContextMenu, { type MenuItem } from '$lib/components/ContextMenu.svelte';
  import ShareExportModal from '$lib/components/share/ShareExportModal.svelte';
  import HiveCreateSheet from '$lib/components/hive/HiveCreateSheet.svelte';
  import StackSection from '$lib/components/stack/StackSection.svelte';
  import StackDots from '$lib/components/stack/StackDots.svelte';
  import WorkspaceDeleteConfirm from './WorkspaceDeleteConfirm.svelte';
  import { stackStore } from '$lib/stores/stack.svelte';
  import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';
  import type { MoveTargetWindow, Workspace } from '$lib/tauri/types';
  import { workspaceMoveItem } from '$lib/stores/windowMove';
  import { isServiceTab, isStripHidden } from '$lib/workspace/hiddenTabs';
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
    // Only on THIS install: `installed` may be left over from an earlier one (the banner's),
    // and restarting on it after this download failed would land the older version.
    if (await updaterStore.downloadAndInstall()) {
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

  /** Tabs in this workspace's strips — the row's count badge (docs/stack.md §7,
   *  docs/hive.md §5). */
  function visibleTabIds(ws: Workspace): string[] {
    return ws.panes.flatMap(p => p.tabs.filter(t => !isStripHidden(t)).map(t => t.id));
  }

  /** Tabs whose state the row's status signals read. Services are excluded — a dev server
   *  writing to its log all day would otherwise hold the activity dot on permanently — but a
   *  strip-hidden Hive agent is not: its waiting prompt must still show here (docs/hive.md §5). */
  function signalTabIds(ws: Workspace): string[] {
    return ws.panes.flatMap(p => p.tabs.filter(t => !isServiceTab(t)).map(t => t.id));
  }

  function workspaceHasActivity(workspaceId: string): boolean {
    if (workspaceId === workspacesStore.activeWorkspaceId) return false;
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (!ws) return false;
    const tabIds = signalTabIds(ws);
    return activityStore.hasAnyActivity(tabIds);
  }

  function workspaceTabState(workspaceId: string): 'alert' | 'question' | null {
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (!ws) return null;
    const tabIds = signalTabIds(ws);
    return activityStore.getWorkspaceTabState(tabIds);
  }

  function workspaceClaudeState(workspaceId: string): WorkspaceClaudeState | null {
    if (workspaceId === workspacesStore.activeWorkspaceId) return null;
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (!ws) return null;
    const tabIds = signalTabIds(ws);
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

  /** Delete one workspace that is already suspended (the confirmation suspends it first). */
  async function doDeleteWorkspace(id: string) {
    if (workspacesStore.workspaces.length > 1) {
      await workspacesStore.deleteWorkspace(id);
    } else {
      // Last workspace: kill terminals and show empty state
      await terminalsStore.killAllTerminals();
      await commands.resetWindow();
      workspacesStore.reset();
    }
  }

  async function handleSuspendAllOthers(e: MouseEvent) {
    e.stopPropagation();
    await workspacesStore.suspendAllOtherWorkspaces();
  }

  // Pointer-based drag reordering (same pattern as TerminalTabs)
  let dragWorkspaceId = $state<string | null>(null);
  /** The ROW dropped on, by id: the list's order on screen (sort order, the suspended group,
   *  nesting) is not the store's, so a screen index can't be used as a store index. */
  let dropTargetId = $state<string | null>(null);
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
    if ((e.target as HTMLElement).closest('.tooltip-wrapper')) return;
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
        dropTargetId = wsEls[i].dataset.workspaceId ?? null;
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
        dropTargetId = wsEls[wsEls.length - 1].dataset.workspaceId ?? null;
        dropSide = 'after';
        foundTarget = true;
      }
    }
    if (!foundTarget) {
      dropTargetId = null;
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

    const targetIndex = dropTargetId ? workspacesStore.workspaces.findIndex(w => w.id === dropTargetId) : -1;
    if (dragWorkspaceId && targetIndex !== -1) {
      const sourceId = dragWorkspaceId;
      const allWs = workspacesStore.workspaces;
      const fromIndex = allWs.findIndex(w => w.id === sourceId);
      const isCopy = e.altKey;

      // Compute the insertion position
      let insertPos = dropSide === 'after' ? targetIndex + 1 : targetIndex;

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
    dropTargetId = null;
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

  // Overlord accessor-row state: how many things want a human, whether anything is
  // being typed right now, and how loud the badge should be.
  // `humanEscalations`, not `escalations`: agent-addressed rows (a tab's reply to a directive,
  // a permission gate routed to the Overlord agent) are filtered off the deck, so counting
  // them here lit "1 item waiting on you" over a board with nothing on it and no way to
  // clear the badge — it only cleared when the AGENT next pulled its queue.
  // ── Stack (docs/stack.md §7) ──
  // ONE stack panel at the foot of the sidebar, showing the active workspace's stack, or the
  // one whose row dots were clicked until the active workspace changes. Opening a stack
  // inline under its row pushed every row below it down under the pointer.
  let stackPanelFor = $state<string | null>(null);
  let stackPanelOpen = $state(true);
  $effect(() => {
    void workspacesStore.activeWorkspaceId;
    untrack(() => { stackPanelFor = null; });
  });
  let stackSection = $state<StackSection | null>(null);
  /** A workspace with an empty stack has no panel to add from; the row's menu asks for
   *  one here so the panel mounts and can open its modal. */
  let addingStackFor = $state<string | null>(null);
  /** Set while one of the panel's forms (add, edit, import, Run on) is open: the panel stays on
   *  that workspace until it closes, so a workspace switch can't unmount the form under the
   *  human's typing. */
  let stackPanelHeld = $state<string | null>(null);
  // A suspended workspace's stack shows too: its services can't start, but they can be read,
  // edited and added to, as they always could.
  const stackPanelWorkspace = $derived.by(() => {
    const id = stackPanelHeld ?? stackPanelFor ?? workspacesStore.activeWorkspaceId;
    const ws = workspacesStore.workspaces.find((w) => w.id === id);
    if (!ws || ws.overlord) return null;
    return stackStore.services(ws.id).length > 0 || addingStackFor === ws.id || stackPanelHeld === ws.id ? ws : null;
  });
  /** Show a workspace's stack in the panel (its row's dots; the menu's Add / Import). */
  function showStack(workspaceId: string) {
    stackPanelFor = workspaceId === workspacesStore.activeWorkspaceId ? null : workspaceId;
    stackPanelOpen = true;
  }
  async function addToStack(workspaceId: string, how: 'add' | 'import') {
    addingStackFor = workspaceId;
    showStack(workspaceId);
    // Once the panel has mounted (or re-keyed) on that workspace: a DOM update, not a frame,
    // so it doesn't wait on a window that isn't being painted.
    await tick();
    if (how === 'add') stackSection?.openAdd(); else stackSection?.openImport();
  }
  let workspaceMenu = $state<{ x: number; y: number; workspaceId: string } | null>(null);
  /** The workspace the Share dialog is open for (docs/workspace-share.md §3). */
  let shareExportFor = $state<string | null>(null);
  /** The workspace the Hive Create sheet is open for (docs/hive.md §3). */
  let hiveSheetFor = $state<string | null>(null);
  // An agent's proposal the human chose to Review (`proposeHive`, lib/hive/proposeHive.ts) opens
  // the sheet on the workspace made for it.
  $effect(() => {
    const open = (e: Event) => { hiveSheetFor = (e as CustomEvent<string>).detail; };
    window.addEventListener('hive-open-create', open);
    return () => window.removeEventListener('hive-open-create', open);
  });
  /** Offered for any workspace that could become one: what stands in the way (a used tab, an
   *  archived one) is the sheet's to say and, where it can, to clear. Hiding the item left the
   *  human with no word on why it was gone. */
  function hiveItems(workspaceId: string): MenuItem[] {
    const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
    if (!ws || ws.hive || ws.overlord) return [];
    return [
      { label: '', separator: true, action: () => {} },
      { label: 'Make this a Hive…', action: () => { hiveSheetFor = workspaceId; } },
    ];
  }
  function workspaceMenuItems(workspaceId: string) {
    const services = stackStore.services(workspaceId);
    // Any copy, on any checkout (docs/hive.md §6.9): Stop stack stops them all.
    const anyLive = stackStore.anyLive(workspaceId);
    const run = (label: string, p: Promise<unknown>) => p.catch((e) => logError(`stack: ${label}: ${e}`));
    const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
    const muted = !!ws?.muted;
    const sep: MenuItem = { label: '', separator: true, action: () => {} };
    return [
      { label: 'Rename', action: () => { if (ws) void startEditing(ws.id, ws.name); } },
      ws?.suspended
        ? { label: 'Resume workspace', action: () => run('resume', handleItemClick(workspaceId)) }
        : { label: 'Suspend workspace', action: () => run('suspend', workspacesStore.suspendWorkspace(workspaceId)) },
      // Overlord exemption (docs/overlord.md §11). A Hive's tabs are never the engine's anyway
      // (docs/hive.md §7.3), so it isn't offered there; but one already set (before "Make this
      // a Hive", say) still turns off its watch scripts, stack tools and follow-up relaunches,
      // so it stays clearable.
      ...(preferencesStore.overlordEnabled && ws && !ws.overlord && (!ws.hive || ws.overlord_exempt)
        ? [{
            label: ws.overlord_exempt ? 'Supervise with Overlord again' : 'Exempt from Overlord',
            action: () => run('exempt', workspacesStore.setWorkspaceOverlordExempt(workspaceId, !ws.overlord_exempt)),
          }]
        : []),
      // Mute (docs/mute.md): nothing from any tab in it notifies — toast, OS, sound, phone push.
      { label: muted ? 'Unmute notifications' : 'Mute notifications', action: () => run(muted ? 'unmute' : 'mute', workspacesStore.setWorkspaceMuted(workspaceId, !muted)) },
      sep,
      { label: 'Stack', action: () => {}, submenu: [
        { label: 'Start stack', disabled: services.length === 0, action: () => run('start stack', stackStore.startStack(workspaceId)) },
        { label: 'Stop stack', disabled: !anyLive, action: () => run('stop stack', stackStore.stopStack(workspaceId)) },
        { label: 'Restart stack', disabled: !anyLive, action: () => run('restart stack', stackStore.stopStack(workspaceId).then(() => stackStore.startStack(workspaceId))) },
        sep,
        { label: 'Add service…', action: () => addToStack(workspaceId, 'add') },
        { label: 'Import from project…', action: () => addToStack(workspaceId, 'import') },
      ] },
      { label: 'Mesh', action: () => {}, submenu: [
        ws?.bridge_all
          ? { label: 'Disable mesh', action: () => run('disable mesh', agentMeshStore.setMeshEnabled(workspaceId, false)) }
          : { label: 'Enable mesh…', action: async () => {
              // The setup modal opens the cockpit on success, and the cockpit shows the ACTIVE
              // workspace — so switch first, as the mesh glyph does.
              if (workspaceId !== workspacesStore.activeWorkspaceId) await handleItemClick(workspaceId);
              window.dispatchEvent(new CustomEvent('open-mesh-setup', { detail: workspaceId }));
            } },
        ...meshLinkItems(workspaceId),
      ] },
      sep,
      workspaceMoveItem(moveTargets, workspaceId),
      { label: 'Share workspace…', action: () => { shareExportFor = workspaceId; } },
      { label: 'Move project…', action: async () => {
        const { relocateStore } = await import('$lib/stores/relocate.svelte');
        const why = await relocateStore.openForWorkspace(workspaceId);
        if (why) {
          const { dispatch } = await import('$lib/stores/notificationDispatch');
          dispatch("Can't move this project", why, 'error');
        }
      } },
      ...hiveItems(workspaceId),
      sep,
      { label: 'Delete workspace…', danger: true, action: () => askDelete(workspaceId) },
    ];
  }

  /** The hold-to-delete confirmation, anchored under the row it deletes. */
  let deleteFor = $state<{ workspaceId: string; anchor: DOMRect } | null>(null);
  function askDelete(workspaceId: string) {
    const row = workspaceListEl?.querySelector<HTMLElement>(`.workspace-item[data-workspace-id="${workspaceId}"]`);
    if (!row) return;
    deleteFor = { workspaceId, anchor: row.getBoundingClientRect() };
  }

  /** Linked meshes (docs/mesh-workspace.md §17): "Link mesh with ›" any other workspace in
   *  this window (a plain one becomes a mesh), and "Unlink mesh" while this one is linked. */
  function meshLinkItems(workspaceId: string) {
    const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
    if (!ws || ws.overlord) return [];
    const fail = async (what: string, why: string) => {
      logError(`mesh: ${what}: ${why}`);
      const { dispatch } = await import('$lib/stores/notificationDispatch');
      dispatch(`Couldn't ${what}`, why, 'error');
    };
    const candidates = agentMeshStore.linkCandidates(workspaceId);
    const items: MenuItem[] = [{
      label: 'Link mesh with',
      disabled: candidates.length === 0,
      action: () => {},
      submenu: candidates.map((c) => ({
        label: c.mesh ? c.name : `${c.name} (becomes a mesh)`,
        action: () => {
          agentMeshStore.linkMeshes(workspaceId, c.id)
            .then((r) => { if ('error' in r) void fail('link the meshes', r.error); })
            .catch((e) => void fail('link the meshes', String(e)));
        },
      })),
    }];
    if (ws.bridge_all && ws.mesh_group) {
      const others = agentMeshStore.linkedWorkspaces(workspaceId).filter((w) => w.id !== workspaceId);
      items.push({
        label: others.length ? `Unlink mesh from ${others.map((w) => w.name).join(', ')}` : 'Unlink mesh',
        action: () => { agentMeshStore.unlinkMesh(workspaceId).catch((e) => void fail('unlink the mesh', String(e))); },
      });
    }
    return items;
  }

  /** Windows the menu's "Move to Window" can send to, fetched as the menu opens. */
  let moveTargets = $state<MoveTargetWindow[] | null>(null);
  function openWorkspaceMenu(e: MouseEvent, workspaceId: string) {
    e.preventDefault();
    openWorkspaceMenuAt(e.clientX, e.clientY, workspaceId);
  }
  function openWorkspaceMenuAt(x: number, y: number, workspaceId: string) {
    moveTargets = null;
    commands.listMoveTargets().then((t) => { moveTargets = t; }).catch((err) => logError(`listMoveTargets: ${err}`));
    workspaceMenu = { x, y, workspaceId };
  }

  // Counts what is ON THE DECK, not what the agent has yet to read. `read` is the Overlord
  // AGENT's delivery receipt (see OverlordEscalation), and this badge is the human's only
  // standing signal that the board has something on it — so reading one as the other meant
  // the supervisor pulling its queue silently zeroed the badge over a deck still holding
  // every card, and nothing brought it back. Badge count and deck row count are the same
  // number by construction now; both clear on dismissal, which is the human's own act.
  const overlordAttention = $derived(
    overlordStore.proposals.length + overlordStore.humanEscalations.length
  );
  const overlordUrgent = $derived(overlordStore.humanEscalations.length > 0);
  const overlordBusy = $derived(overlordStore.ritualProgress.length > 0);

  async function handleOverlordClick() {
    await workspacesStore.ensureOverlordWorkspace();
  }

  const sortedWorkspaces = $derived.by(() => {
    // The Overlord workspace lives behind its own accessor row, never in the list.
    const ws = workspacesStore.workspaces.filter(w => !w.overlord);
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

  /** Suspended workspaces sit in their own group at the bottom of the list; the active one
   *  stays among the live ones even while it is suspended, so the row you're on never jumps. */
  const liveWorkspaces = $derived(sortedWorkspaces.filter((w) => !w.suspended || w.id === workspacesStore.activeWorkspaceId));
  const suspendedWorkspaces = $derived(sortedWorkspaces.filter((w) => w.suspended && w.id !== workspacesStore.activeWorkspaceId));
  let suspendedOpen = $state(false);

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
    <span class="titlebar-end">
      {#if preferencesStore.fileTreeEnabled}
        <!-- Files opens right beside this edge, so its toggle lives here rather than only behind
             a shortcut; +page carries one in the titlebar for while the sidebar is collapsed. -->
        <span class="files-toggle">
          <IconButton
            tooltip={`${workspacesStore.fileTreeOpen ? 'Hide' : 'Show'} Files (${modSymbol}⇧B)`}
            size={20}
            active={workspacesStore.fileTreeOpen}
            aria-pressed={workspacesStore.fileTreeOpen}
            onclick={() => fileTreeStore.toggleColumn()}
          >
            <Icon name="folder" size={12} />
          </IconButton>
          {#if fileTreeStore.pointerWaiting}
            <!-- An agent pointed at a file while Files was closed (revealInTree never opens it). -->
            <span class="files-pointed" aria-hidden="true"></span>
          {/if}
        </span>
      {/if}
      <IconButton tooltip="Collapse sidebar ({modSymbol}B)" size={20} style="font-size: 1.231rem" onclick={() => workspacesStore.toggleSidebar()}>&#x2039;</IconButton>
    </span>
  </div>
  {#if preferencesStore.overlordEnabled}
    <Tooltip block text={overlordAttention > 0
      ? `Overlord — ${overlordAttention} item${overlordAttention === 1 ? '' : 's'} waiting on you`
      : 'Overlord — the Loom, triage, board and ledger'}>
    <button
      class="overlord-row"
      class:active={workspacesStore.activeWorkspace?.overlord}
      onclick={handleOverlordClick}
    >
      <span class="overlord-glyph">♔</span>
      <span class="overlord-title">Overlord</span>
      {#if overlordBusy}
        <span class="ov-dot ov-dot-live" style:--tone="var(--accent)"></span>
      {/if}
      {#if overlordAttention > 0}
        <span class="overlord-badge" class:urgent={overlordUrgent}>{overlordAttention}</span>
      {/if}
    </button>
    </Tooltip>
  {/if}
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
    {#snippet wsRow(workspace: Workspace)}
      <div
        class="workspace-item"
        class:active={workspace.id === workspacesStore.activeWorkspaceId}
        class:suspended={!workspaceIsLive(workspace)}
        class:import-highlight={workspace.import_highlight}
        class:dragging={dragWorkspaceId === workspace.id}
        class:drop-before={dropTargetId === workspace.id && dropSide === 'before' && dragWorkspaceId !== workspace.id}
        class:drop-after={dropTargetId === workspace.id && dropSide === 'after' && dragWorkspaceId !== workspace.id}
        data-workspace-id={workspace.id}
        onclick={() => handleItemClick(workspace.id)}
        oncontextmenu={(e) => openWorkspaceMenu(e, workspace.id)}
        ondblclick={() => startEditing(workspace.id, workspace.name)}
        onpointerdown={(e) => handlePointerDown(e, workspace.id)}
        onpointermove={handlePointerMove}
        onpointerup={handlePointerUp}
        role="button"
        tabindex="0"
        onkeydown={(e) => {
          if (e.target !== e.currentTarget) return;
          if (e.key === 'Enter') handleItemClick(workspace.id);
          // The menu from the keyboard, as everywhere on macOS and Windows.
          else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
            e.preventDefault();
            const r = e.currentTarget.getBoundingClientRect();
            openWorkspaceMenuAt(r.left + 24, r.bottom, workspace.id);
          }
        }}
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
            <span class="tab-count-badge" class:active={workspace.id === workspacesStore.activeWorkspaceId} class:status-alert={wsTabState === 'alert'} class:status-question={wsTabState === 'question'} class:status-claude-active={!wsTabState && wsClaude === 'active'} class:status-claude-idle={!wsTabState && wsClaude === 'idle-unread'} class:status-claude-idle-read={!wsTabState && wsClaude === 'idle-read'} class:status-activity={!wsTabState && !wsClaude && wsActivity}>{visibleTabIds(workspace).length}</span>
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
              {:else if wsActivity}
                <StatusDot color="dim" />
              {/if}
            </span>
          {/if}
          <!-- Fixed columns, the same on every row: status · name · stack · flags · kind. Every
               action is in the right-click menu, so nothing appears or moves under the pointer. -->
          <span class="workspace-name">{workspace.name}</span>
          <!-- Flags sit left of the dots in one right-aligned column, so the dots end at the same
               x on every row without a column reserved for flags most rows don't have. -->
          <span class="ws-signals">
            {#if workspace.overlord_exempt}
              <!-- Always shown while set: an exemption you can't see is one you forget you set,
                   and then wonder why the fleet is missing a workspace (docs/overlord.md §11). -->
              <Tooltip text="Exempt from Overlord. Right-click → Supervise with Overlord again">
                <span class="ws-flag exempt"><Icon name="eye-off" size={11} /></span>
              </Tooltip>
            {/if}
            {#if workspace.muted}
              <Tooltip text="Muted: no tab here sends notifications. Right-click → Unmute notifications">
                <span class="ws-flag"><Icon name="bell-off" size={11} /></span>
              </Tooltip>
            {/if}
            <StackDots
              {workspace}
              shown={stackPanelOpen && stackPanelWorkspace?.id === workspace.id}
              onclick={() => showStack(workspace.id)}
            />
          </span>
          <span class="ws-kind">
            {#if workspace.hive}
              <!-- Not a button: opening the workspace already opens its Hive (docs/hive.md §4.1). -->
              <Tooltip text="Hive: a coordinator and its workers">
                <span class="ws-glyph hive"><Icon name="hive" size={12} /></span>
              </Tooltip>
            {:else if workspace.bridge_all}
              {@const linked = agentMeshStore.isLinkedMesh(workspace.id)}
              <Tooltip text={`${linked ? 'Linked mesh' : 'Mesh workspace'}: open the cockpit (${modSymbol}⇧M)`}>
                <button
                  type="button"
                  class="ws-glyph mesh"
                  class:linked
                  aria-label="Open mesh cockpit"
                  onclick={(e) => { e.stopPropagation(); if (workspace.id !== workspacesStore.activeWorkspaceId) handleItemClick(workspace.id); window.dispatchEvent(new CustomEvent('open-mesh-cockpit')); }}
                ><Icon name={linked ? 'mesh-linked' : 'mesh'} size={12} /></button>
              </Tooltip>
            {/if}
          </span>
        {/if}
      </div>
    {/snippet}
    {#each liveWorkspaces as workspace (workspace.id)}
      {@render wsRow(workspace)}
    {/each}
    {#if suspendedWorkspaces.length}
      <!-- Suspended workspaces wait at the bottom, named while the group is closed. -->
      <button
        type="button"
        class="suspended-group"
        aria-expanded={suspendedOpen}
        onclick={() => { suspendedOpen = !suspendedOpen; }}
      >
        <span class="chev" class:open={suspendedOpen}>›</span>
        <span class="suspended-label">Suspended ({suspendedWorkspaces.length})</span>
        {#if !suspendedOpen}
          <span class="suspended-names">{suspendedWorkspaces.map((w) => w.name).join(', ')}</span>
        {/if}
      </button>
      {#if suspendedOpen}
        {#each suspendedWorkspaces as workspace (workspace.id)}
          {@render wsRow(workspace)}
        {/each}
      {/if}
    {/if}
  </div>

  {#if stackPanelWorkspace}
    {@const panelWs = stackPanelWorkspace}
    {@const activeWs = workspacesStore.activeWorkspace}
    {#key panelWs.id}
      <StackSection
        bind:this={stackSection}
        workspace={panelWs}
        expanded={stackPanelOpen}
        ontoggle={() => { stackPanelOpen = !stackPanelOpen; }}
        onsettled={() => { if (addingStackFor === panelWs.id) addingStackFor = null; }}
        onformchange={(open) => { stackPanelHeld = open ? panelWs.id : null; }}
        back={stackPanelFor && activeWs && !activeWs.overlord && stackStore.services(activeWs.id).length > 0
          ? { name: activeWs.name, onclick: () => { stackPanelFor = null; } }
          : null}
      />
    {/key}
  {/if}

  {#if deleteFor}
    {@const target = workspacesStore.workspaces.find((w) => w.id === deleteFor?.workspaceId)}
    {#if target}
      {#key target.id}
        <WorkspaceDeleteConfirm
          workspace={target}
          anchor={deleteFor.anchor}
          remove={doDeleteWorkspace}
          onclose={() => { deleteFor = null; }}
        />
      {/key}
    {/if}
  {/if}
  {#if workspaceMenu}
    <ContextMenu items={workspaceMenuItems(workspaceMenu.workspaceId)} x={workspaceMenu.x} y={workspaceMenu.y} onclose={() => (workspaceMenu = null)} />
  {/if}
  {#if shareExportFor}
    <ShareExportModal workspaceId={shareExportFor} onclose={() => (shareExportFor = null)} />
  {/if}
  {#if hiveSheetFor}
    <!-- Keyed: a proposal opened over an open sheet (`hive-open-create`) is a new sheet with its
         own seed, never the old one's fields under the new workspace's id. The old one keeps its
         draft as it unmounts, and its late close (a Create still finishing) closes only itself. -->
    {#key hiveSheetFor}
      <HiveCreateSheet workspaceId={hiveSheetFor} onclose={(closed) => { if (hiveSheetFor === closed) hiveSheetFor = null; }} />
    {/key}
  {/if}

  {#if updaterStore.showBanner}
    <div class="update-banner">
      {#if !updaterStore.restarting}
        <button class="update-dismiss" onclick={() => updaterStore.dismiss()} aria-label="Dismiss">&times;</button>
      {/if}
      {#if updaterStore.restarting}
        <div class="update-text">Saving state and restarting…</div>
      {:else if updaterStore.installed}
        <div class="update-text">Update installed</div>
        <button class="update-action" onclick={() => updaterStore.restart()}>Restart</button>
      {:else if updaterStore.installing}
        <div class="update-text">Installing v{updaterStore.version}…</div>
      {:else if updaterStore.downloading}
        <div class="update-text">
          Downloading v{updaterStore.version}…
          {#if updaterStore.totalBytes}
            {Math.floor(updaterStore.downloadedBytes / updaterStore.totalBytes * 100)}%
          {:else if updaterStore.downloadedBytes}
            {(updaterStore.downloadedBytes / 1048576).toFixed(1)} MB
          {/if}
        </div>
      {:else}
        <div class="update-text">
          v{updaterStore.version} available
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
  oninstall={updaterStore.installed ? () => updaterStore.restart() : updaterStore.currentUpdate ? handleInstallFromModal : undefined}
  installLabel={updaterStore.restarting ? 'Restarting…' : updaterStore.installed ? 'Restart Now' : rechecking ? 'Checking…' : updaterStore.installing ? 'Installing…' : updaterStore.downloading ? 'Downloading…' : 'Install & Restart'}
  installDisabled={rechecking || updaterStore.downloading || updaterStore.restarting}
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

  /* At the 215px minimum the strip is full: the DEV badge alone overran it, and the Files
     toggle adds 22px. The version number is the one thing that gives way (ellipsis), so
     the chevron and the toggle are never pushed past the edge. */
  .titlebar-end {
    margin-left: auto;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .files-toggle {
    position: relative;
    display: flex;
  }

  .files-pointed {
    position: absolute;
    top: 1px;
    right: 1px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    pointer-events: none;
  }

  /* The full wordmark, not a compact mark: the emblem is illegible at this size
     (mesh collapses to specks below ~32px), so the sidebar carries the name. 13px
     is the ceiling — at 70px wide it leaves the DEV + version badges and the
     collapse chevron room inside the 183px content box of a 215px sidebar.

     No opacity here, unlike the loading and empty-pane logos: the wordmark is
     duo-tone, and a multiplier pulls both tones toward the background together,
     so at 13px "Term" stopped reading as a different colour from "mai" at all
     (3.95:1 against the background at 0.7, 6.67:1 at full). The other two slots
     keep theirs — they are meant to be ghosts, not identity. */
  .sidebar-logo {
    height: 13px;
    aspect-ratio: 1661 / 320;
    flex-shrink: 0;
    pointer-events: none;
    background: var(--logo-url, url(/logo-light.png)) center / contain no-repeat;
  }

  .dev-badge {
    margin-left: 6px;
    flex-shrink: 0;
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
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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

  /* Overlord accessor — deck-styled (see src/lib/overlord/deck.css), sitting above the
     workspace list because the Overlord workspace is not one of them. */
  .overlord-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    padding: 11px 16px;
    background:
      linear-gradient(90deg,
        color-mix(in srgb, var(--accent) 7%, transparent) 0%, transparent 55%);
    border: none;
    border-bottom: 1px solid var(--bg-light);
    color: var(--fg-dim);
    cursor: pointer;
    text-align: left;
    transition: color 0.15s ease, background 0.15s ease;
  }

  /* Accent bar that grows in on hover/active — the deck's "live rail" motif. */
  .overlord-row::before {
    content: '';
    position: absolute;
    left: 0; top: 0; bottom: 0;
    width: 2px;
    background: var(--accent);
    transform: scaleY(0);
    transform-origin: center;
    transition: transform 0.18s cubic-bezier(0.3, 0.8, 0.3, 1);
  }
  .overlord-row:hover::before,
  .overlord-row.active::before { transform: scaleY(1); }

  .overlord-row:hover { color: var(--fg); }
  .overlord-row.active {
    color: var(--fg);
    background:
      linear-gradient(90deg,
        color-mix(in srgb, var(--accent) 16%, transparent) 0%, transparent 70%);
  }

  .overlord-glyph {
    font-size: 1rem;
    line-height: 1;
    color: var(--accent);
    text-shadow: 0 0 10px color-mix(in srgb, var(--accent) 45%, transparent);
  }

  .overlord-title {
    flex: 1;
    font-size: 0.846rem;
    font-weight: 600;
    letter-spacing: 0.5px;
    text-transform: uppercase;
  }

  .overlord-badge {
    font-family: var(--ov-mono);
    background: color-mix(in srgb, var(--accent) 20%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 55%, transparent);
    color: var(--accent);
    border-radius: 2px;
    padding: 0 6px;
    font-size: 0.692rem;
    font-weight: 600;
    line-height: 15px;
    min-width: 16px;
    text-align: center;
  }
  .overlord-badge.urgent {
    background: color-mix(in srgb, var(--red) 22%, transparent);
    border-color: color-mix(in srgb, var(--red) 60%, transparent);
    color: var(--red);
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

  .suspended-group {
    align-items: baseline;
    background: none;
    border: none;
    border-top: 1px solid var(--bg-light);
    color: var(--fg-dim);
    cursor: pointer;
    display: flex;
    font-family: inherit;
    font-size: 0.8rem;
    gap: 6px;
    margin-top: 6px;
    min-width: 0;
    padding: 8px 16px;
    text-align: left;
    width: 100%;
  }
  .suspended-group:hover { color: var(--fg); }
  .suspended-group:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .suspended-group .chev {
    display: inline-block;
    flex-shrink: 0;
    transition: transform 0.12s;
    width: 8px;
  }
  .suspended-group .chev.open { transform: rotate(90deg); }
  .suspended-label { flex-shrink: 0; }
  /* One line, ellipsised: the full list is one click away, and a wrapped list of names here
     would read as rows. */
  .suspended-names {
    min-width: 0;
    opacity: 0.7;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The same columns on every row — status · name · flags and stack dots · kind — so each kind
     of signal sits in the same place down the list and nothing reflows on hover. */
  .workspace-item {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto 18px;
    align-items: center;
    padding: 6px 10px 6px 16px;
    min-height: 32px;
    box-sizing: border-box;
    cursor: pointer;
    transition: background 0.1s;
    column-gap: 6px;
  }
  .workspace-item .edit-input { grid-column: 1 / -1; }

  .ws-signals, .ws-kind {
    display: flex;
    align-items: center;
    justify-content: flex-end;
  }
  .ws-signals { gap: 4px; }
  .ws-flag { display: flex; color: var(--fg-dim); }
  .ws-flag.exempt { color: var(--yellow); }

  .ws-glyph {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: none;
    color: var(--fg-dim);
  }
  .ws-glyph.mesh { cursor: pointer; }
  .ws-glyph.mesh:hover { color: var(--fg); background: color-mix(in srgb, var(--fg) 10%, transparent); }
  .ws-glyph.mesh.linked { color: var(--cyan, var(--accent)); }
  .ws-glyph.hive { color: var(--yellow, #e0af68); }
  .ws-glyph:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }

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

  /* Opacity, not just --fg-dim: how far --fg-dim sits from --fg is the theme's choice, and in
     Catppuccin Latte (#4c4f69 vs #6c6f85) a colour swap alone is invisible. Suspended tabs
     dim by opacity (TerminalTabs), which reads in every theme; this matches them. */
  .workspace-item.suspended .workspace-name {
    color: var(--fg-dim);
    opacity: 0.5;
  }

  .workspace-item.suspended:hover .workspace-name,
  .workspace-item.suspended.active .workspace-name {
    opacity: 0.75;
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
    min-width: 0;
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

  .edit-input {
    min-width: 0;
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
    width: 24px;
    height: 24px;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  /* Each occupied slot is a bare dot whose click target fills the whole 24px
     slot (matching the toolbar icon buttons). Fixed size, not 100% — the
     Tooltip wrapper is inline-flex and would otherwise collapse the button to
     the dot's 6px. The wrapper shrink-wraps the button, so it grows to 24px. */
  .footer-agent-dot {
    display: flex;
    align-items: center;
    justify-content: center;
    box-sizing: border-box;
    width: 24px;
    height: 24px;
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
