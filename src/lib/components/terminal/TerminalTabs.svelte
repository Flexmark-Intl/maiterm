<script lang="ts">
  import { tick, onDestroy, untrack } from 'svelte';
  import type { Tab, Pane } from '$lib/tauri/types';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { activityStore } from '$lib/stores/activity.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import type { OscState } from '$lib/stores/terminals.svelte';
  import { modLabel, isModKey } from '$lib/utils/platform';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { getCompiledTitlePatterns, extractDirFromTitle } from '$lib/utils/promptPattern';
  import { onVariablesChange, interpolateVariables } from '$lib/stores/triggers.svelte';
  import { isEditorDirty } from '$lib/stores/editorRegistry.svelte';
  import { getBridgeStatus } from '$lib/stores/sshMcpBridge.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
  import { sshDisconnectStore } from '$lib/stores/sshDisconnect.svelte';
  import { isImageFile, isPdfFile } from '$lib/utils/languageDetect';
  import Icon from '$lib/components/Icon.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import TabListMenu from './TabListMenu.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';

  interface Props {
    workspaceId: string;
    pane: Pane;
  }

  let { workspaceId, pane }: Props = $props();

  let archiveDropdownOpen = $state(false);
  let archiveDropdownEl = $state<HTMLElement | null>(null);
  let archiveDropdownPos = $state<{ top: number; left?: number; right?: number }>({ top: 0, left: 0 });
  const archivedTabs = $derived(
    [...(workspacesStore.workspaces.find(w => w.id === workspaceId)?.archived_tabs ?? [])]
      .sort((a, b) => {
        const aTime = a.archived_at ? new Date(a.archived_at).getTime() : 0;
        const bTime = b.archived_at ? new Date(b.archived_at).getTime() : 0;
        return bTime - aTime; // most recent first
      })
  );
  const archiveItems = $derived(
    archivedTabs.map(t => ({
      tab: t,
      label: t.archived_name ?? t.name,
      meta: t.archived_at ? relativeTime(t.archived_at) : null,
    }))
  );

  // Overflow menu: tabs scrolled out of view in the bar (not fully visible).
  let overflowDropdownOpen = $state(false);
  let overflowDropdownEl = $state<HTMLElement | null>(null);
  let overflowDropdownPos = $state<{ top: number; left?: number; right?: number }>({ top: 0 });
  let overflowTabIds = $state<Set<string>>(new Set());

  let editingId = $state<string | null>(null);
  let editingName = $state('');
  let editingOriginalName = '';
  let editInput = $state<HTMLInputElement | null>(null);

  // Track OSC titles for tabs in this pane.
  // Seed from existing terminal state so titles survive component recreation
  // (e.g., workspace switch destroys and recreates SplitPane → TerminalTabs).
  let oscTitles = $state<Map<string, string>>(new Map());
  // svelte-ignore state_referenced_locally -- intentional one-time seed from existing terminal state; live updates come from onOscChange subscription below
  for (const tab of pane.tabs) {
    const osc = terminalsStore.getOsc(tab.id);
    // svelte-ignore state_referenced_locally
    if (osc?.title) oscTitles.set(tab.id, osc.title);
  }

  const unsubOsc = terminalsStore.onOscChange((tabId: string, osc: OscState) => {
    if (osc.title && pane.tabs.some(t => t.id === tabId)) {
      oscTitles = new Map(oscTitles);
      oscTitles.set(tabId, osc.title);
    }
  });
  onDestroy(unsubOsc);

  // Track modifier key for "modifier" tab button style.
  // Only register listeners when the preference is active.
  let modHeld = $state(false);
  $effect(() => {
    if (preferencesStore.tabButtonStyle !== 'modifier') {
      modHeld = false;
      return;
    }
    function onKeyDown(e: KeyboardEvent) { if (isModKey(e)) modHeld = true; }
    function onKeyUp(e: KeyboardEvent) { if (!e.metaKey && !e.ctrlKey) modHeld = false; }
    function onBlur() { modHeld = false; }
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('keyup', onKeyUp);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('keyup', onKeyUp);
      window.removeEventListener('blur', onBlur);
    };
  });

  // Display-order tabs: pinned tabs always cluster at the front (in their
  // drag-orderable storage order), then — when groupActiveTabs is on — active
  // (non-suspended) tabs, then suspended tabs, each preserving relative order.
  // Pinned tabs are exempt from the active/suspended split: they hold their slot
  // regardless of liveness.
  const groupedTabs = $derived.by(() => {
    const pinned = pane.tabs.filter(t => t.pinned);
    const rest = pane.tabs.filter(t => !t.pinned);
    if (!preferencesStore.groupActiveTabs) {
      // No active-grouping — but pinned still cluster at the front.
      if (pinned.length === 0) return { tabs: pane.tabs, activeCount: 0, pinnedCount: 0 };
      return { tabs: [...pinned, ...rest], activeCount: 0, pinnedCount: pinned.length };
    }
    // Read instanceVersion to re-derive when terminals register/unregister
    void terminalsStore.instanceVersion;
    const active: Tab[] = [];
    const suspended: Tab[] = [];
    for (const tab of rest) {
      const isTerminal = tab.tab_type === 'terminal' || !tab.tab_type;
      if (isTerminal && !terminalsStore.get(tab.id) && !terminalsStore.isSpawning(tab.id)) {
        suspended.push(tab);
      } else {
        active.push(tab);
      }
    }
    return {
      tabs: [...pinned, ...active, ...suspended],
      // Divider sits before the first suspended (unpinned) tab — after the pinned
      // cluster and the unpinned-active group.
      activeCount: suspended.length > 0 ? pinned.length + active.length : 0,
      pinnedCount: pinned.length,
    };
  });
  const displayTabs = $derived(groupedTabs.tabs);
  const activeGroupCount = $derived(groupedTabs.activeCount);
  const pinnedCount = $derived(groupedTabs.pinnedCount);

  // Tabs scrolled out of view (not fully visible) in the bar, in display order.
  const overflowTabs = $derived(displayTabs.filter(t => overflowTabIds.has(t.id)));
  const overflowItems = $derived(overflowTabs.map(t => {
    // Only suspended terminals carry an "age" — how long since they were
    // suspended. Live (active) tabs and viewers have none.
    const isTerm = t.tab_type === 'terminal' || !t.tab_type;
    const isLive = !!(terminalsStore.get(t.id) || terminalsStore.isSpawning(t.id));
    const meta = isTerm && !isLive && t.suspended_at ? relativeTime(t.suspended_at) : null;
    return { tab: t, label: displayName(t), meta };
  }));

  // When grouping first turns on, auto-switch away from a suspended tab to the first active one.
  // Only fires on the groupActiveTabs toggle, not on every active tab change.
  let prevGrouping = preferencesStore.groupActiveTabs;
  $effect(() => {
    const grouping = preferencesStore.groupActiveTabs;
    const wasOff = !prevGrouping;
    prevGrouping = grouping;
    if (!grouping || !wasOff) return;
    const activeTabId = pane.active_tab_id;
    if (!activeTabId) return;
    const activeTab = pane.tabs.find(t => t.id === activeTabId);
    if (!activeTab) return;
    const isTerminal = activeTab.tab_type === 'terminal' || !activeTab.tab_type;
    if (isTerminal && !terminalsStore.get(activeTabId)) {
      // Current tab is suspended — switch to first active (non-suspended) tab if one exists
      const firstActive = groupedTabs.tabs.find(t => {
        const isTerm = t.tab_type === 'terminal' || !t.tab_type;
        return !isTerm || terminalsStore.get(t.id);
      });
      if (firstActive && firstActive.id !== activeTabId) {
        workspacesStore.setActiveTab(workspaceId, pane.id, firstActive.id);
      }
    }
  });

  // When group-active-tabs is on, persist a tab's visual jump into the active
  // group: the moment a previously-suspended tab goes live again, move it in
  // storage to the end of the active group (just before the first suspended
  // tab) — where it already shows. This keeps drag order meaningful and lets
  // recently-used tabs settle at the front, so a later suspend-all leaves them
  // where they were. Keyed on instanceVersion (bumps on register/unregister).
  //
  // `everLive` distinguishes a real resume (was live, suspended, now live) from
  // the initial lazy-spawn on app load and from brand-new tabs (createTab keeps
  // its own placement next to the active tab) — neither should be promoted.
  const everLive = new Set<string>();
  let prevLive = new Set<string>();
  let liveSeeded = false;
  // A suspended tab activated under active-grouping gets reordered into the
  // active group once its PTY registers (below). The pre-promotion scroll is
  // deferred until then via this guard so we scroll to the final slot, not the
  // tab's old suspended-group position. Set in handleTabClick, consumed here.
  let pendingPromoteScrollId: string | null = null;
  // Tabs the user explicitly unsuspended by clicking. The `everLive` gate skips
  // a tab's first live transition (so the passive lazy-spawn on app load isn't
  // reordered), but a deliberate click IS a resume and must promote even the
  // first time — otherwise re-suspending it drops it back to its old slot.
  const userResumedIds = new Set<string>();
  // For click-driven unsuspends, the tab the user was on at click time. The
  // promotion below places the resumed tab right after this anchor (when the
  // anchor is still in the active group) instead of at the end of the group.
  const resumeAnchorIds = new Map<string, string>();
  $effect(() => {
    void terminalsStore.instanceVersion;
    const grouping = preferencesStore.groupActiveTabs;
    untrack(() => {
      const isTerminal = (t: Tab) => t.tab_type === 'terminal' || !t.tab_type;
      const liveNow = new Set(
        pane.tabs
          .filter(t => isTerminal(t) && (terminalsStore.get(t.id) || terminalsStore.isSpawning(t.id)))
          .map(t => t.id)
      );
      const resumed: { id: string; anchor: string | null }[] = [];
      for (const t of pane.tabs) {
        // Only tabs that just went live this tick are candidates.
        if (!isTerminal(t) || !liveNow.has(t.id) || prevLive.has(t.id)) continue;
        // Consume the archive-restore marker on this first live transition
        // (whether or not grouping is on), so it never leaks into a later
        // suspend→resume cycle of the same tab. A restored tab keeps the
        // placement restoreArchivedTab gave it — don't promote it.
        const justRestored = terminalsStore.consumeRestoredFromArchive(t.id);
        // Promote on a real resume: either the tab was live before this session
        // (everLive), or the user just clicked it to unsuspend (userResumed).
        const userResumed = userResumedIds.delete(t.id);
        const anchor = resumeAnchorIds.get(t.id) ?? null;
        resumeAnchorIds.delete(t.id);
        if (liveSeeded && grouping && (everLive.has(t.id) || userResumed) && !justRestored) {
          resumed.push({ id: t.id, anchor });
        }
      }
      for (const id of liveNow) everLive.add(id);
      prevLive = liveNow;
      liveSeeded = true;
      for (const { id, anchor } of resumed) workspacesStore.promoteResumedTab(workspaceId, pane.id, id, anchor);
      // Once the just-activated tab is live (and any promotion reorder above has
      // landed), scroll it into view — handleTabClick deferred this scroll. Skip
      // if the user has since switched away (their new tab already scrolled).
      if (pendingPromoteScrollId && liveNow.has(pendingPromoteScrollId)) {
        if (pendingPromoteScrollId === pane.active_tab_id) scrollTabIntoView(pendingPromoteScrollId);
        pendingPromoteScrollId = null;
      }
    });
  });

  // Track trigger variable changes for reactive tab title updates
  let varVersion = $state(0);
  const unsubVars = onVariablesChange((tabId: string) => {
    if (pane.tabs.some(t => t.id === tabId)) {
      varVersion++;
    }
  });
  onDestroy(unsubVars);

  function displayName(tab: Tab): string {
    // Read varVersion to subscribe this derived value to variable changes
    void varVersion;
    if (tab.custom_name) {
      let result = tab.name;
      if (result.includes('%title') || result.includes('%dir')) {
        const oscTitle = oscTitles.get(tab.id);
        if (!oscTitle && !result.includes('%')) return result;
        if (oscTitle) {
          if (result.includes('%title')) result = result.replace('%title', oscTitle);
          if (result.includes('%dir')) {
            const patterns = getCompiledTitlePatterns(preferencesStore.promptPatterns);
            result = result.replace('%dir', extractDirFromTitle(oscTitle, patterns));
          }
        }
      }
      // Interpolate %varName from trigger variables
      if (result.includes('%')) {
        result = interpolateVariables(tab.id, result, true);
      }
      return result;
    }
    return oscTitles.get(tab.id) ?? tab.name;
  }

  async function startEditing(tab: Tab, e: MouseEvent) {
    e.stopPropagation();
    if (editingId === tab.id) return; // Already editing — let browser handle word selection
    editingId = tab.id;
    editingName = tab.custom_name ? tab.name : displayName(tab);
    editingOriginalName = editingName;
    await tick();
    editInput?.select();
  }

  async function finishEditing() {
    if (editingId) {
      const trimmed = editingName.trim();
      if (trimmed) {
        // Skip rename if nothing changed — preserves original custom_name state
        if (trimmed !== editingOriginalName) {
          await workspacesStore.renameTab(workspaceId, pane.id, editingId, trimmed, true);
        }
      } else {
        // Clearing the name resets to default (auto-naming from OSC title)
        const oscTitle = terminalsStore.getOsc(editingId)?.title;
        const defaultName = oscTitle ?? 'Terminal';
        await workspacesStore.renameTab(workspaceId, pane.id, editingId, defaultName, false);
        // Populate oscTitles so displayName picks it up immediately
        if (oscTitle) {
          oscTitles = new Map(oscTitles);
          oscTitles.set(editingId, oscTitle);
        }
      }
    }
    editingId = null;
    editingName = '';
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      // Blur first so focus moves off the input before it's removed from DOM.
      // This prevents the browser from scrolling the tabs bar when the focused
      // element disappears. The blur triggers finishEditing via onblur.
      editInput?.blur();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      editInput?.blur();
      editingId = null;
      editingName = '';
    }
  }

  function relativeTime(iso: string | null): string {
    if (!iso) return '';
    const diff = Date.now() - new Date(iso).getTime();
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return 'just now';
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    if (days === 1) return 'yesterday';
    if (days < 30) return `${days}d ago`;
    return new Date(iso).toLocaleDateString();
  }

  async function handleNewTab() {
    const count = pane.tabs.length + 1;
    await workspacesStore.createTab(workspaceId, pane.id, `Terminal ${count}`, { append: true });
  }

  function handleReconnect(tabId: string, e: MouseEvent) {
    e.stopPropagation();
    handleTabClick(tabId);
    window.dispatchEvent(new CustomEvent('ssh-reconnect', { detail: { tabId } }));
  }

  async function handleArchiveTab(tabId: string, e: MouseEvent) {
    e.stopPropagation();
    const tab = pane.tabs.find(t => t.id === tabId);
    if (!tab) return;
    const name = displayName(tab);
    const ws = workspacesStore.activeWorkspace;

    if (pane.tabs.length > 1) {
      await workspacesStore.archiveTab(workspaceId, pane.id, tabId, name);
    } else if (ws && ws.panes.length > 1) {
      // Last tab in pane — archive then delete pane
      await workspacesStore.archiveTab(workspaceId, pane.id, tabId, name);
      await workspacesStore.deletePane(workspaceId, pane.id);
    } else {
      // Last tab in last pane — archive then create fresh tab
      await workspacesStore.archiveTab(workspaceId, pane.id, tabId, name);
      await workspacesStore.createTab(workspaceId, pane.id, 'Terminal 1');
    }
  }

  async function handleRestoreArchivedTab(tabId: string) {
    await workspacesStore.restoreArchivedTab(workspaceId, tabId);
    archiveDropdownOpen = false;
  }

  async function handleDeleteArchivedTab(tabId: string, e: MouseEvent) {
    e.stopPropagation();
    await workspacesStore.deleteArchivedTab(workspaceId, tabId);
  }

  function handleArchiveDropdownClickOutside(e: MouseEvent) {
    if (archiveDropdownEl && !archiveDropdownEl.contains(e.target as Node)) {
      archiveDropdownOpen = false;
    }
  }

  function handleArchiveDropdownKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') archiveDropdownOpen = false;
  }

  $effect(() => {
    if (archiveDropdownOpen) {
      document.addEventListener('click', handleArchiveDropdownClickOutside, true);
      document.addEventListener('keydown', handleArchiveDropdownKeydown);
      return () => {
        document.removeEventListener('click', handleArchiveDropdownClickOutside, true);
        document.removeEventListener('keydown', handleArchiveDropdownKeydown);
      };
    }
  });

  // Recompute which tabs are scrolled out of view. Cheap geometry check against
  // the scroll viewport; a tab counts as overflowed when either edge is clipped.
  function computeOverflow() {
    if (!tabsBarEl || tabsBarEl.clientWidth === 0) {
      if (overflowTabIds.size) overflowTabIds = new Set();
      return;
    }
    const barRect = tabsBarEl.getBoundingClientRect();
    const next = new Set<string>();
    for (const el of tabsBarEl.querySelectorAll<HTMLElement>('.tab[data-tab-id]')) {
      const r = el.getBoundingClientRect();
      if (r.left < barRect.left - 1 || r.right > barRect.right + 1) {
        const id = el.dataset.tabId;
        if (id) next.add(id);
      }
    }
    if (next.size !== overflowTabIds.size || [...next].some(id => !overflowTabIds.has(id))) {
      overflowTabIds = next;
    }
  }

  async function handleOverflowActivate(tabId: string) {
    await handleTabClick(tabId);
    overflowDropdownOpen = false;
  }

  function handleOverflowDropdownClickOutside(e: MouseEvent) {
    if (overflowDropdownEl && !overflowDropdownEl.contains(e.target as Node)) {
      overflowDropdownOpen = false;
    }
  }

  function handleOverflowDropdownKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') overflowDropdownOpen = false;
  }

  $effect(() => {
    if (overflowDropdownOpen) {
      document.addEventListener('click', handleOverflowDropdownClickOutside, true);
      document.addEventListener('keydown', handleOverflowDropdownKeydown);
      return () => {
        document.removeEventListener('click', handleOverflowDropdownClickOutside, true);
        document.removeEventListener('keydown', handleOverflowDropdownKeydown);
      };
    }
  });

  // Close the overflow menu once nothing is left hidden (e.g. after archiving
  // or closing the last overflowed tab from within the menu).
  $effect(() => {
    if (overflowDropdownOpen && overflowTabs.length === 0) overflowDropdownOpen = false;
  });

  async function handleDuplicateTab(tabId: string, e: MouseEvent) {
    e.stopPropagation();
    await workspacesStore.duplicateTab(workspaceId, pane.id, tabId, { shallow: e.altKey });
  }

  async function handleSuspendTab(tabId: string, e: MouseEvent) {
    e.stopPropagation();
    await workspacesStore.suspendTab(workspaceId, pane.id, tabId);
  }

  async function handleTogglePin(tabId: string, e: MouseEvent) {
    e.stopPropagation();
    const tab = pane.tabs.find(t => t.id === tabId);
    if (!tab) return;
    await workspacesStore.setTabPinned(workspaceId, pane.id, tabId, !(tab.pinned ?? false));
  }

  async function handleCloseTab(tabId: string, e: MouseEvent) {
    e.stopPropagation();
    const ws = workspacesStore.activeWorkspace;
    if (pane.tabs.length > 1) {
      await workspacesStore.deleteTab(workspaceId, pane.id, tabId);
    } else if (ws && ws.panes.length > 1) {
      // Last tab in pane — close the pane
      await workspacesStore.deletePane(workspaceId, pane.id);
    } else {
      // Last tab in last pane — close tab, pane shows empty state
      await workspacesStore.deleteTab(workspaceId, pane.id, tabId);
    }
  }

  async function handleTabClick(tabId: string) {
    // The tab the user is leaving — anchor for placing a resumed suspended tab.
    const prevActive = pane.active_tab_id;
    // Clicking a tab also focuses its pane, so pane-targeted actions (Cmd+T,
    // Cmd+D split, etc.) operate on the pane the user just interacted with.
    if (workspacesStore.activeWorkspace?.active_pane_id !== pane.id) {
      await workspacesStore.setActivePane(workspaceId, pane.id);
    }
    await workspacesStore.setActiveTab(workspaceId, pane.id, tabId);
    // A suspended terminal under active-grouping will be reordered into the
    // active group once its PTY registers; defer the scroll to the promotion
    // effect so it lands on the final slot rather than the suspended-group spot.
    const tab = pane.tabs.find(t => t.id === tabId);
    const isTerm = !!tab && (tab.tab_type === 'terminal' || !tab.tab_type);
    const isLive = !!(terminalsStore.get(tabId) || terminalsStore.isSpawning(tabId));
    if (isTerm && !isLive && preferencesStore.groupActiveTabs) {
      pendingPromoteScrollId = tabId;
      userResumedIds.add(tabId); // a deliberate unsuspend must promote even on its first live
      // Remember where the user came from so the resumed tab lands right next
      // to it (promoteResumedTab validates the anchor is still active-group).
      if (prevActive && prevActive !== tabId) resumeAnchorIds.set(tabId, prevActive);
      else resumeAnchorIds.delete(tabId);
    } else {
      scrollTabIntoView(tabId);
    }
  }

  function scrollTabIntoView(tabId: string) {
    requestAnimationFrame(() => {
      const el = tabsBarEl?.querySelector<HTMLElement>(`[data-tab-id="${tabId}"]`);
      if (!el || !tabsBarEl || tabsBarEl.clientWidth === 0) return;
      const barRect = tabsBarEl.getBoundingClientRect();
      const tabRect = el.getBoundingClientRect();
      // If tab is fully visible, do nothing
      if (tabRect.left >= barRect.left && tabRect.right <= barRect.right) return;
      // el.offsetLeft is relative to the nearest positioned ancestor (BODY here, not the bar),
      // so derive the tab's position within the bar's scrollable content from the rects.
      const tabContentLeft = tabRect.left - barRect.left + tabsBarEl.scrollLeft;
      const target = tabContentLeft + tabRect.width / 2 - tabsBarEl.clientWidth / 2;
      const maxScroll = Math.max(0, tabsBarEl.scrollWidth - tabsBarEl.clientWidth);
      tabsBarEl.scrollTo({ left: Math.max(0, Math.min(target, maxScroll)), behavior: 'smooth' });
    });
  }

  // Pointer-based drag reordering (HTML5 drag-and-drop is unreliable in Tauri WKWebView)
  let dragTabId = $state<string | null>(null);
  let dropTargetIndex = $state<number | null>(null);
  let dropSide = $state<'before' | 'after'>('before');
  let dropWorkspaceId: string | null = null;
  // Drop target: another pane's tab bar (move tab into that pane).
  // beforeTabId null = append at end.
  let dropPane: { paneId: string; beforeTabId: string | null } | null = null;
  let foreignDropEl: HTMLElement | null = null;    // foreign tab carrying drop-before/after
  let foreignDropBarEl: HTMLElement | null = null; // foreign bar carrying drop-target-bar
  // Drop target: a pane's body — edges create a new split, center moves into the pane.
  type DropEdge = 'left' | 'right' | 'top' | 'bottom' | 'center';
  let dropSplit: { paneId: string; edge: DropEdge } | null = null;
  let splitOverlay: HTMLElement | null = null;
  // Tab right-click menu (move to split / other panes)
  let tabContextMenu = $state<{ x: number; y: number; tabId: string } | null>(null);

  const DRAG_THRESHOLD = 5;
  let dragStartX = 0;
  let dragStartY = 0;
  let lastPointerX = 0;
  let lastPointerY = 0;
  let pendingDragTabId: string | null = null;
  let justDragged = false;
  let ghost: HTMLElement | null = null;
  let cursorBadge: HTMLElement | null = null;
  let tabsBarEl: HTMLElement;

  // Scroll active tab into view when it changes (e.g. Cmd+1-9 shortcuts).
  // Track previous ID so renames (which replace pane objects) don't re-trigger.
  let prevActiveTabId: string | null = null;
  $effect(() => {
    const activeId = pane.active_tab_id;
    // Skip the tab whose scroll is deferred to the promotion effect (a resumed
    // suspended tab) — scrolling now would target its pre-promotion position.
    if (activeId && activeId !== prevActiveTabId && activeId !== pendingPromoteScrollId) {
      scrollTabIntoView(activeId);
    }
    prevActiveTabId = activeId ?? null;
  });

  // Keep overflow state fresh when the bar resizes (window/sidebar/notes changes).
  $effect(() => {
    if (!tabsBarEl) return;
    const ro = new ResizeObserver(() => computeOverflow());
    ro.observe(tabsBarEl);
    return () => ro.disconnect();
  });

  // Recompute overflow whenever the tab set, grouping, or active tab changes.
  // rAF lets the DOM settle (widths/positions) before measuring.
  $effect(() => {
    void displayTabs;
    void activeGroupCount;
    void pane.active_tab_id;
    const raf = requestAnimationFrame(computeOverflow);
    return () => cancelAnimationFrame(raf);
  });

  function handlePointerDown(e: PointerEvent, tabId: string) {
    // Only primary button, skip if editing or clicking close button
    if (e.button !== 0 || editingId === tabId) return;
    if ((e.target as HTMLElement).closest('.tab-actions')) return;
    // Alt+click tab → shallow duplicate (name, cwd, history, variables only)
    if (e.altKey) {
      e.preventDefault();
      workspacesStore.duplicateTab(workspaceId, pane.id, tabId, { shallow: true });
      return;
    }
    pendingDragTabId = tabId;
    dragStartX = e.clientX;
    dragStartY = e.clientY;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function handlePointerMove(e: PointerEvent) {
    if (!pendingDragTabId && !dragTabId) return;

    // Check threshold before starting drag
    if (pendingDragTabId && !dragTabId) {
      const dx = e.clientX - dragStartX;
      const dy = e.clientY - dragStartY;
      if (Math.abs(dx) < DRAG_THRESHOLD && Math.abs(dy) < DRAG_THRESHOLD) return;
      dragTabId = pendingDragTabId;
      pendingDragTabId = null;
      createGhost(e);
    }

    if (!dragTabId || !ghost) return;

    // Move ghost
    ghost.style.left = `${e.clientX}px`;
    ghost.style.top = `${e.clientY}px`;

    // Hit-test tab elements to find drop target
    const tabEls = tabsBarEl.querySelectorAll<HTMLElement>('.tab');
    let foundTabTarget = false;
    for (let i = 0; i < tabEls.length; i++) {
      const rect = tabEls[i].getBoundingClientRect();
      if (e.clientX >= rect.left && e.clientX <= rect.right &&
          e.clientY >= rect.top && e.clientY <= rect.bottom) {
        const midX = rect.left + rect.width / 2;
        dropSide = e.clientX < midX ? 'before' : 'after';
        dropTargetIndex = i;
        foundTabTarget = true;
        break;
      }
    }
    if (!foundTabTarget) {
      dropTargetIndex = null;
    }

    // Hit-test workspace sidebar elements
    const wsEls = document.querySelectorAll<HTMLElement>('[data-workspace-id]');
    let foundWsId: string | null = null;
    for (const wsEl of wsEls) {
      const rect = wsEl.getBoundingClientRect();
      if (e.clientX >= rect.left && e.clientX <= rect.right &&
          e.clientY >= rect.top && e.clientY <= rect.bottom) {
        const wsId = wsEl.getAttribute('data-workspace-id');
        if (wsId && wsId !== workspaceId) {
          foundWsId = wsId;
        }
        break;
      }
    }

    // Update drop-target class on workspace elements
    if (foundWsId !== dropWorkspaceId) {
      // Remove old highlight
      if (dropWorkspaceId) {
        const oldEl = document.querySelector(`[data-workspace-id="${dropWorkspaceId}"]`);
        oldEl?.classList.remove('drop-target');
      }
      // Add new highlight
      if (foundWsId) {
        const newEl = document.querySelector(`[data-workspace-id="${foundWsId}"]`);
        newEl?.classList.add('drop-target');
        // Clear tab drop target when over a workspace
        dropTargetIndex = null;
      }
      dropWorkspaceId = foundWsId;
    }

    // Hit-test other panes' tab bars and pane bodies (same-workspace moves)
    updatePaneDropTargets(e, foundTabTarget, !!foundWsId);

    lastPointerX = e.clientX;
    lastPointerY = e.clientY;
    updateCursorBadge(e.altKey);
  }

  function updatePaneDropTargets(e: PointerEvent, overOwnTab: boolean, overSidebar: boolean) {
    let nextPane: { paneId: string; beforeTabId: string | null } | null = null;
    let nextForeignEl: HTMLElement | null = null;
    let nextForeignSide: 'before' | 'after' = 'before';
    let nextForeignBarEl: HTMLElement | null = null;
    let nextSplit: { paneId: string; edge: DropEdge } | null = null;

    if (!overOwnTab && !overSidebar) {
      // Foreign tab bars — move the tab into that pane at the hovered position
      let overAnyBar = false;
      for (const barEl of document.querySelectorAll<HTMLElement>('.tabs-bar[data-pane-id]')) {
        const rect = barEl.getBoundingClientRect();
        if (rect.width === 0 || e.clientX < rect.left || e.clientX > rect.right ||
            e.clientY < rect.top || e.clientY > rect.bottom) continue;
        overAnyBar = true;
        const barPaneId = barEl.getAttribute('data-pane-id');
        if (!barPaneId || barPaneId === pane.id) break; // own bar: reorder logic owns it
        const tabEls = Array.from(barEl.querySelectorAll<HTMLElement>('.tab[data-tab-id]'));
        let beforeTabId: string | null = null;
        for (let i = 0; i < tabEls.length; i++) {
          const r = tabEls[i].getBoundingClientRect();
          if (e.clientX >= r.left && e.clientX <= r.right) {
            const after = e.clientX >= r.left + r.width / 2;
            nextForeignEl = tabEls[i];
            nextForeignSide = after ? 'after' : 'before';
            beforeTabId = after
              ? tabEls[i + 1]?.getAttribute('data-tab-id') ?? null
              : tabEls[i].getAttribute('data-tab-id');
            break;
          }
        }
        if (!nextForeignEl) nextForeignBarEl = barEl; // empty bar area → append
        nextPane = { paneId: barPaneId, beforeTabId };
        break;
      }

      // Pane bodies — edge zones create a new split, center moves into the pane
      if (!overAnyBar && !nextPane) {
        for (const paneEl of document.querySelectorAll<HTMLElement>('.split-pane[data-pane-id]')) {
          const rect = paneEl.getBoundingClientRect();
          if (rect.width === 0 || e.clientX < rect.left || e.clientX > rect.right ||
              e.clientY < rect.top || e.clientY > rect.bottom) continue;
          const paneId = paneEl.getAttribute('data-pane-id');
          if (!paneId) break;
          const rx = (e.clientX - rect.left) / rect.width;
          const ry = (e.clientY - rect.top) / rect.height;
          const edges: Array<[DropEdge, number]> = [['left', rx], ['right', 1 - rx], ['top', ry], ['bottom', 1 - ry]];
          edges.sort((a, b) => a[1] - b[1]);
          const edge: DropEdge = edges[0][1] < 0.3 ? edges[0][0] : 'center';
          const isOwnPane = paneId === pane.id;
          // Dropping a tab in its own pane's center is a no-op; splitting a
          // pane off with its only tab just churns pane IDs.
          if (isOwnPane && (edge === 'center' || pane.tabs.length === 1)) break;
          nextSplit = { paneId, edge };
          break;
        }
      }
    }

    if (foreignDropEl && foreignDropEl !== nextForeignEl) {
      foreignDropEl.classList.remove('drop-before', 'drop-after');
    }
    foreignDropEl = nextForeignEl;
    if (foreignDropEl) {
      foreignDropEl.classList.toggle('drop-before', nextForeignSide === 'before');
      foreignDropEl.classList.toggle('drop-after', nextForeignSide === 'after');
    }

    if (foreignDropBarEl && foreignDropBarEl !== nextForeignBarEl) {
      foreignDropBarEl.classList.remove('drop-target-bar');
    }
    foreignDropBarEl = nextForeignBarEl;
    foreignDropBarEl?.classList.add('drop-target-bar');

    dropPane = nextPane;
    dropSplit = nextSplit;
    updateSplitOverlay();
  }

  function updateSplitOverlay() {
    if (!dropSplit) {
      splitOverlay?.remove();
      splitOverlay = null;
      return;
    }
    const paneEl = document.querySelector<HTMLElement>(`.split-pane[data-pane-id="${dropSplit.paneId}"]`);
    if (!paneEl) return;
    const r = paneEl.getBoundingClientRect();
    let left = r.left, top = r.top, width = r.width, height = r.height;
    if (dropSplit.edge === 'left') width = r.width / 2;
    else if (dropSplit.edge === 'right') { left += r.width / 2; width = r.width / 2; }
    else if (dropSplit.edge === 'top') height = r.height / 2;
    else if (dropSplit.edge === 'bottom') { top += r.height / 2; height = r.height / 2; }
    if (!splitOverlay) {
      splitOverlay = document.createElement('div');
      splitOverlay.className = 'split-drop-overlay';
      document.body.appendChild(splitOverlay);
    }
    splitOverlay.style.left = `${left}px`;
    splitOverlay.style.top = `${top}px`;
    splitOverlay.style.width = `${width}px`;
    splitOverlay.style.height = `${height}px`;
  }

  function updateCursorBadge(altKey: boolean) {
    if (!cursorBadge) return;
    cursorBadge.style.left = `${lastPointerX + 16}px`;
    cursorBadge.style.top = `${lastPointerY + 16}px`;
    if (dropWorkspaceId && altKey) {
      cursorBadge.style.display = 'flex';
    } else {
      cursorBadge.style.display = 'none';
    }
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

    const wasDragging = !!dragTabId;

    if (dragTabId && dropWorkspaceId) {
      // Drop onto a workspace — copy (Alt/Option) or move
      const tabId = dragTabId;
      const targetWsId = dropWorkspaceId;
      const isCopy = e.altKey;
      clearDragState();
      if (isCopy) {
        workspacesStore.copyTabToWorkspace(workspaceId, pane.id, tabId, targetWsId);
      } else {
        workspacesStore.moveTabToWorkspace(workspaceId, pane.id, tabId, targetWsId);
      }
      return;
    }

    if (dragTabId && dropPane) {
      // Drop onto another pane's tab bar — move the tab there, PTY intact
      const tabId = dragTabId;
      const { paneId: targetPaneId, beforeTabId } = dropPane;
      clearDragState();
      workspacesStore.moveTabToPane(workspaceId, pane.id, tabId, targetPaneId, beforeTabId);
      return;
    }

    if (dragTabId && dropSplit) {
      // Drop onto a pane body — edge creates a new split, center moves into it
      const tabId = dragTabId;
      const { paneId: targetPaneId, edge } = dropSplit;
      clearDragState();
      if (edge === 'center') {
        workspacesStore.moveTabToPane(workspaceId, pane.id, tabId, targetPaneId);
      } else {
        const direction = edge === 'left' || edge === 'right' ? 'horizontal' : 'vertical';
        const before = edge === 'left' || edge === 'top';
        workspacesStore.moveTabToSplit(workspaceId, pane.id, tabId, targetPaneId, direction, before);
      }
      return;
    }

    if (dragTabId && dropTargetIndex !== null) {
      // Use displayTabs for index mapping since that's what the DOM reflects
      const displayed = displayTabs;
      const fromIndex = displayed.findIndex(t => t.id === dragTabId);
      if (fromIndex !== -1) {
        let toIndex = dropSide === 'after' ? dropTargetIndex + 1 : dropTargetIndex;
        // Keep the pin boundary intact: a pinned tab can only land within the
        // pinned cluster (display indices [0, pinnedCount)), and an unpinned tab
        // can't slip into it. Without this, a cross-boundary drop would just snap
        // back on the next derive.
        if (displayed[fromIndex]?.pinned) toIndex = Math.min(toIndex, pinnedCount);
        else toIndex = Math.max(toIndex, pinnedCount);
        if (fromIndex < toIndex) toIndex--;
        if (fromIndex !== toIndex) {
          const ids = displayed.map(t => t.id);
          const [moved] = ids.splice(fromIndex, 1);
          ids.splice(toIndex, 0, moved);
          workspacesStore.reorderTabs(workspaceId, pane.id, ids);
        }
      }
    }

    clearDragState();

    // After any drag, re-focus the active terminal. During the drag the pointer
    // capture moves focus away from the xterm canvas, and the DOM reorder of
    // slot elements can corrupt xterm.js rendering. Wait for Svelte to settle
    // the DOM, then refresh + focus.
    if (wasDragging && pane.active_tab_id) {
      justDragged = true;
      // Clear flag after the click event that follows pointerup
      requestAnimationFrame(() => { justDragged = false; });
      tick().then(() => {
        const instance = terminalsStore.get(pane.active_tab_id!);
        if (instance) {
          instance.terminal.refresh(0, instance.terminal.rows - 1);
          instance.terminal.focus();
        }
      });
    }
  }

  function createGhost(e: PointerEvent) {
    const sourceTab = tabsBarEl.querySelector<HTMLElement>(`.tab[data-tab-id="${dragTabId}"]`);
    if (!sourceTab) return;
    ghost = sourceTab.cloneNode(true) as HTMLElement;
    ghost.classList.add('drag-ghost');
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
    dragTabId = null;
    dropTargetIndex = null;
    pendingDragTabId = null;
    if (dropWorkspaceId) {
      const el = document.querySelector(`[data-workspace-id="${dropWorkspaceId}"]`);
      el?.classList.remove('drop-target');
      dropWorkspaceId = null;
    }
    dropPane = null;
    dropSplit = null;
    if (foreignDropEl) {
      foreignDropEl.classList.remove('drop-before', 'drop-after');
      foreignDropEl = null;
    }
    if (foreignDropBarEl) {
      foreignDropBarEl.classList.remove('drop-target-bar');
      foreignDropBarEl = null;
    }
    if (splitOverlay) {
      splitOverlay.remove();
      splitOverlay = null;
    }
    if (ghost) {
      ghost.remove();
      ghost = null;
    }
    if (cursorBadge) {
      cursorBadge.remove();
      cursorBadge = null;
    }
  }

  function tabMenuItems(tabId: string) {
    const onlyTab = pane.tabs.length === 1;
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    const otherPanes = (ws?.panes ?? []).filter(p => p.id !== pane.id);
    const tabObj = pane.tabs.find(t => t.id === tabId);
    const isPinned = !!tabObj?.pinned;
    const isTerminalTab = (tabObj?.tab_type ?? 'terminal') === 'terminal';
    const isAgentTab = !!tabObj?.runtime;
    const exposeAll = preferencesStore.mailinkExposeAll;
    const isExcluded = !!tabObj?.mailink_excluded;
    const isNative = !!tabObj?.mailink_native;
    type MenuItem = { label: string; action: () => void; disabled?: boolean; separator?: boolean };
    // maiLink availability item (docs/mailink-protocol.md). Two modes:
    //  · expose-all (default): agent tabs are available automatically — offer to hold one back.
    //    (A downed agent keeps its runtime, so it stays available for auto-resume from the phone.)
    //  · designate-only: user hand-picks tabs — offer to add/remove (works for plain shells too).
    let mailinkItem: MenuItem | null = null;
    if (isTerminalTab) {
      if (exposeAll) {
        if (isAgentTab) {
          mailinkItem = {
            label: isExcluded ? 'Make available in maiLink' : 'Make unavailable in maiLink',
            action: () => workspacesStore.setTabMailinkExcluded(workspaceId, pane.id, tabId, !isExcluded),
          };
        }
      } else {
        mailinkItem = {
          label: isNative ? 'Make unavailable in maiLink' : 'Make available in maiLink',
          action: () => workspacesStore.setTabMailinkNative(workspaceId, pane.id, tabId, !isNative),
        };
      }
    }
    const items: Array<MenuItem> = [
      {
        label: isPinned ? 'Unpin tab' : 'Pin tab',
        action: () => workspacesStore.setTabPinned(workspaceId, pane.id, tabId, !isPinned),
      },
      ...(mailinkItem ? [mailinkItem] : []),
      { label: '', separator: true, action: () => {} },
      {
        label: 'Move to New Split Right',
        disabled: onlyTab,
        action: () => workspacesStore.moveTabToSplit(workspaceId, pane.id, tabId, pane.id, 'horizontal'),
      },
      {
        label: 'Move to New Split Down',
        disabled: onlyTab,
        action: () => workspacesStore.moveTabToSplit(workspaceId, pane.id, tabId, pane.id, 'vertical'),
      },
    ];
    if (otherPanes.length > 0) {
      items.push({ label: '', separator: true, action: () => {} });
      for (const p of otherPanes) {
        const activeTab = p.tabs.find(t => t.id === p.active_tab_id) ?? p.tabs[0];
        const hint = activeTab ? ` (${activeTab.name})` : '';
        items.push({
          label: `Move to ${p.name}${hint}`,
          action: () => workspacesStore.moveTabToPane(workspaceId, pane.id, tabId, p.id),
        });
      }
    }
    return items;
  }
</script>

<!-- Blue pin glyph shown in a pinned tab's leading indicator slot. Clicking it unpins. -->
{#snippet pinGlyph(t: Tab)}
  <Tooltip text="Unpin tab"><button class="indicator pin-indicator" onclick={(e) => handleTogglePin(t.id, e)} aria-label="Unpin tab"><Icon name="pin" size={11} /></button></Tooltip>
{/snippet}

<div class="tabs-bar" data-tauri-drag-region data-pane-id={pane.id}>
    <div class="tabbar-menu-wrapper" bind:this={archiveDropdownEl}>
      <Tooltip text={archivedTabs.length > 0 ? `Archived tabs (${archivedTabs.length})` : 'No archived tabs'}>
        <button
          class="tabbar-menu-btn"
          disabled={archivedTabs.length === 0}
          onclick={(e) => {
            e.stopPropagation();
            if (!archiveDropdownOpen) {
              const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
              archiveDropdownPos = { top: rect.bottom + 2, left: rect.left };
            }
            archiveDropdownOpen = !archiveDropdownOpen;
          }}
        >
          <Icon name="archive" size={12} />{#if archivedTabs.length > 0} {archivedTabs.length}{/if}
        </button>
      </Tooltip>
      {#if archiveDropdownOpen}
        <TabListMenu
          items={archiveItems}
          position={archiveDropdownPos}
          searchable
          onActivate={(t) => handleRestoreArchivedTab(t.id)}
        >
          {#snippet actions(t)}
            <IconButton tooltip="Restore" onclick={() => handleRestoreArchivedTab(t.id)}><Icon name="restore" size={12} /></IconButton>
            <IconButton tooltip="Delete permanently" danger onclick={(e) => handleDeleteArchivedTab(t.id, e)}><Icon name="close" size={12} /></IconButton>
          {/snippet}
        </TabListMenu>
      {/if}
    </div>

  <div class="tabs-scroll" bind:this={tabsBarEl}
    onwheel={(e) => { if (tabsBarEl) { e.preventDefault(); tabsBarEl.scrollLeft += e.deltaY || e.deltaX; } }}
    onscroll={computeOverflow}
  >
  {#each displayTabs as tab, index (tab.id)}
    {#if activeGroupCount > 0 && index === activeGroupCount}
      <div class="group-divider"></div>
    {/if}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    {@const isEditor = tab.tab_type === 'editor'}
    {@const isDiff = tab.tab_type === 'diff'}
    {@const isSuspendedTab = activeGroupCount > 0 && index >= activeGroupCount}
    {@const shellState = !isEditor && tab.id !== pane.active_tab_id ? activityStore.getShellState(tab.id) : undefined}
    {@const hasActivity = !isEditor && tab.id !== pane.active_tab_id && activityStore.hasActivity(tab.id)}
    {@const tabState = !isEditor && tab.id !== pane.active_tab_id ? activityStore.getTabState(tab.id) : undefined}
    {@const claudeState = !isEditor ? claudeStateStore.getState(tab.id) : undefined}
    {@const sshDropped = !isEditor ? sshDisconnectStore.isDisconnected(tab.id) : false}
    <div
      class="tab"
      class:tab-suspended={isSuspendedTab}
      class:active={tab.id === pane.active_tab_id}
      class:unclamped={editingId === tab.id || tab.custom_name || oscTitles.has(tab.id)}
      class:activity={!shellState && !tabState && hasActivity}
      class:completed={!tabState && shellState?.state === 'completed' && shellState?.exitCode === 0}
      class:failed={!tabState && shellState?.state === 'completed' && shellState?.exitCode !== 0}
      class:tab-alert={tabState === 'alert'}
      class:tab-question={tabState === 'question'}
      class:import-highlight={tab.import_highlight}
      class:dragging={dragTabId === tab.id}
      class:buttons-always={preferencesStore.tabButtonStyle === 'always'}
      class:buttons-never={preferencesStore.tabButtonStyle === 'never'}
      class:buttons-modifier={preferencesStore.tabButtonStyle === 'modifier'}
      class:mod-held={preferencesStore.tabButtonStyle === 'modifier' && modHeld}
      class:drop-before={dropTargetIndex === index && dropSide === 'before' && dragTabId !== tab.id}
      class:drop-after={dropTargetIndex === index && dropSide === 'after' && dragTabId !== tab.id}
      data-tab-id={tab.id}
      onclick={() => { if (!dragTabId && !justDragged) handleTabClick(tab.id); }}
      ondblclick={(e) => startEditing(tab, e)}
      oncontextmenu={(e) => {
        if (editingId === tab.id) return; // native menu for the rename input
        e.preventDefault();
        e.stopPropagation();
        tabContextMenu = { x: e.clientX, y: e.clientY, tabId: tab.id };
      }}
      onpointerdown={(e) => handlePointerDown(e, tab.id)}
      onpointermove={handlePointerMove}
      onpointerup={handlePointerUp}
      role="tab"
      tabindex="0"
      aria-selected={tab.id === pane.active_tab_id}
      onkeydown={(e) => e.key === 'Enter' && handleTabClick(tab.id)}
    >
      {#if editingId === tab.id}
        <div class="edit-wrapper">
          <span class="edit-sizer">{editingName || ' '}</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            type="text"
            size="1"
            bind:value={editingName}
            bind:this={editInput}
            onblur={finishEditing}
            onkeydown={handleKeydown}
            class="edit-input"
            placeholder="%title, %dir, or %varName for dynamic name"
            autofocus
          />
        </div>
      {:else}
        <!-- Pin renders alongside the type/status indicator — pinned-ness must stay
             visible instead of competing with a transient state for the slot. -->
        {#if tab.pinned}{@render pinGlyph(tab)}{/if}
        {#if isDiff}
          <Tooltip text="Diff"><span class="editor-icon"><Icon name="diff" size={12} /></span></Tooltip>
        {:else if isEditor}
          {#if tab.editor_file && isPdfFile(tab.editor_file.file_path)}
            <Tooltip text="PDF"><span class="editor-icon"><Icon name="pdf" size={12} /></span></Tooltip>
          {:else if tab.editor_file && isImageFile(tab.editor_file.file_path)}
            <Tooltip text="Image"><span class="editor-icon"><Icon name="image" size={12} /></span></Tooltip>
          {:else}
            <Tooltip text={isEditorDirty(tab.id) ? 'Unsaved changes' : 'Editor'}><span class="editor-icon" class:editor-dirty={isEditorDirty(tab.id)}><Icon name="file" size={12} /></span></Tooltip>
          {/if}
        {:else if sshDropped}
          <Tooltip text={`SSH disconnected${sshDisconnectStore.getInfo(tab.id)?.host ? ' from ' + sshDisconnectStore.getInfo(tab.id)?.host : ''} — click to reconnect`}><button class="indicator ssh-disconnected" onclick={(e) => handleReconnect(tab.id, e)} aria-label="Reconnect SSH"><Icon name="restore" size={11} /></button></Tooltip>
        {:else if tabState === 'alert'}
          <span class="indicator alert-indicator"><Icon name="warning" size={11} /></span>
        {:else if tabState === 'question'}
          <span class="indicator question-indicator"><Icon name="help" size={11} /></span>
        {:else if claudeState?.state === 'permission'}
          <Tooltip text="Claude needs permission"><span class="indicator claude-permission"><Icon name="warning" size={11} /></span></Tooltip>
        {:else if claudeState?.state === 'active'}
          <Tooltip text={claudeState.toolName ? `Claude: ${claudeState.toolName}${claudeState.toolDetail ? ': ' + claudeState.toolDetail : ''}` : 'Claude is working'}><span class="indicator claude-active"><Icon name="circle" size={10} /></span></Tooltip>
        {:else if claudeState?.state === 'idle'}
          <Tooltip text={claudeState.read ? 'Claude finished (seen)' : 'Claude waiting for input'}><span class="indicator claude-idle"><Icon name={claudeState.read ? 'circle-outline' : 'circle'} size={10} /></span></Tooltip>
        {:else if shellState?.state === 'completed'}
          <span class="indicator" class:completed-indicator={shellState.exitCode === 0} class:failed-indicator={shellState.exitCode !== 0}>{#if shellState.exitCode === 0}<Icon name="check" size={11} />{:else}<Icon name="cross" size={11} />{/if}</span>
        {:else if hasActivity}
          <span class="indicator"><StatusDot color="accent" /></span>
        {/if}
        {#if !isEditor && preferencesStore.claudeCodeIde && preferencesStore.claudeCodeIdeSsh}
          {@const bridgeStatus = getBridgeStatus(tab.id)}
          {#if bridgeStatus}
            <Tooltip text={bridgeStatus === 'connected' ? 'MCP bridge active' : bridgeStatus === 'pending' ? 'MCP bridge connecting\u2026' : 'MCP bridge failed'}><span
              class="bridge-indicator"
              class:bridge-connected={bridgeStatus === 'connected'}
              class:bridge-pending={bridgeStatus === 'pending'}
              class:bridge-failed={bridgeStatus === 'failed'}
            ><Icon name="bolt" size={12} /></span></Tooltip>
          {/if}
        {/if}
        {#if !isEditor && tab.auto_resume_enabled && (tab.auto_resume_ssh_command || tab.auto_resume_cwd || tab.auto_resume_command)}
          <Tooltip text={
            tab.auto_resume_ssh_command
              ? `Auto-resume: ${tab.auto_resume_ssh_command}${tab.auto_resume_remote_cwd ? ` (${tab.auto_resume_remote_cwd})` : ''}`
              : `Auto-resume: ${tab.auto_resume_cwd ?? 'enabled'}`
          }><span class="auto-resume-indicator"><Icon name="resume" size={12} /></span></Tooltip>
        {/if}
        {#if !isEditor && agentBridgeStore.isBridged(tab.id)}
          <Tooltip text={`Bridged to ${agentBridgeStore.getPartnerLabel(tab.id) ?? 'an agent'} — they can message this agent`}><span class="agent-bridge-indicator">⇄</span></Tooltip>
        {/if}
        <span class="tab-name">{displayName(tab)}</span>
        {@const hasRunningPty = !isEditor && !isDiff && !!terminalsStore.get(tab.id)}
        <div class="tab-actions" class:always-visible={preferencesStore.tabButtonStyle === 'always'} class:modifier-only={preferencesStore.tabButtonStyle === 'modifier'} class:modifier-active={preferencesStore.tabButtonStyle === 'modifier' && modHeld} class:never-visible={preferencesStore.tabButtonStyle === 'never'} class:triple-action={isEditor || isDiff} class:quadruple-action={!isEditor && !isDiff && !hasRunningPty} class:quintuple-action={hasRunningPty}>
          <IconButton
            tooltip={tab.pinned ? 'Unpin tab' : 'Pin tab'}
            style="width:22px;height:18px;border-radius:3px"
            onclick={(e) => handleTogglePin(tab.id, e)}
          ><span class="pin-action" class:pinned={tab.pinned}><Icon name="pin" size={11} /></span></IconButton>
          <IconButton
            tooltip="Archive tab"
            style="width:22px;height:18px;border-radius:3px"
            onclick={(e) => handleArchiveTab(tab.id, e)}
          ><Icon name="archive" size={11} /></IconButton>
          {#if !isEditor && !isDiff}
            <IconButton
              tooltip="Duplicate tab ({modLabel}+Shift+T)"
              style="width:22px;height:18px;border-radius:3px"
              onclick={(e) => handleDuplicateTab(tab.id, e)}
            ><Icon name="duplicate" size={11} /></IconButton>
            {#if terminalsStore.get(tab.id)}
              <IconButton
                tooltip="Suspend tab"
                style="width:22px;height:18px;border-radius:3px"
                onclick={(e) => handleSuspendTab(tab.id, e)}
              ><Icon name="pause" size={11} /></IconButton>
            {/if}
          {/if}
          <IconButton
            tooltip="Close tab ({modLabel}+W)"
            style="width:22px;height:18px;border-radius:3px"
            onclick={(e) => handleCloseTab(tab.id, e)}
          >
            <Icon name="close" size={11} />
          </IconButton>
        </div>
      {/if}
    </div>
  {/each}
  </div>

  <Tooltip text="New tab ({modLabel}+T)"><button class="new-tab-btn" onclick={handleNewTab}>
    <Icon name="plus" size={14} />
  </button></Tooltip>

  <div class="tabbar-menu-wrapper" bind:this={overflowDropdownEl}>
    <Tooltip text={overflowTabs.length > 0 ? `Hidden tabs (${overflowTabs.length})` : 'No hidden tabs'}>
      <button
        class="tabbar-menu-btn"
        disabled={overflowTabs.length === 0}
        onclick={(e) => {
          e.stopPropagation();
          if (!overflowDropdownOpen) {
            computeOverflow();
            const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
            overflowDropdownPos = { top: rect.bottom + 2, right: window.innerWidth - rect.right };
          }
          overflowDropdownOpen = !overflowDropdownOpen;
        }}
      >
        <Icon name="list" size={14} />{#if overflowTabs.length > 0} {overflowTabs.length}{/if}
      </button>
    </Tooltip>
    {#if overflowDropdownOpen}
      <TabListMenu
        items={overflowItems}
        position={overflowDropdownPos}
        searchable
        onActivate={(t) => handleOverflowActivate(t.id)}
      >
        {#snippet actions(t)}
          <IconButton tooltip="Archive tab" onclick={(e) => handleArchiveTab(t.id, e)}><Icon name="archive" size={12} /></IconButton>
          <IconButton tooltip="Close tab" danger onclick={(e) => handleCloseTab(t.id, e)}><Icon name="close" size={12} /></IconButton>
        {/snippet}
      </TabListMenu>
    {/if}
  </div>

  {#if pane.active_tab_id}
    <IconButton
      tooltip="Toggle notes ({modLabel}+E)"
      size={26}
      style="margin-right:4px;flex-shrink:0;-webkit-app-region:no-drag"
      onclick={() => workspacesStore.toggleNotes(pane.active_tab_id!)}
    >
      <Icon name="notes" />
    </IconButton>
  {/if}
</div>

{#if tabContextMenu}
  <ContextMenu
    items={tabMenuItems(tabContextMenu.tabId)}
    x={tabContextMenu.x}
    y={tabContextMenu.y}
    onclose={() => tabContextMenu = null}
  />
{/if}

<style>
  .tabs-bar {
    display: flex;
    align-items: center;
    height: var(--tab-height);
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
    padding: 0 4px;
    gap: 2px;
    -webkit-app-region: drag;
    overflow: hidden;
  }

  .tabs-scroll {
    display: flex;
    align-items: center;
    gap: 2px;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
    flex: 1 1 0;
    min-width: 0;
  }

  .tabs-scroll::-webkit-scrollbar {
    display: none;
  }

  .group-divider {
    flex-shrink: 0;
    width: 1px;
    height: 16px;
    background: var(--bg-light);
    margin: 0 4px;
    opacity: 0.6;
  }

  .tab.tab-suspended {
    opacity: 0.45;
  }

  .tab.tab-suspended:hover {
    opacity: 0.7;
  }

  .tab.tab-suspended.active {
    opacity: 1;
  }

  .tab {
    display: flex;
    align-items: center;
    gap: 0;
    padding: 5px 10px;
    border: 1px solid var(--tab-border);
    border-radius: 4px;
    cursor: pointer;
    max-width: 180px;
    transition: background 0.1s, padding-right 0.15s ease, border-color 0.1s;
    -webkit-app-region: no-drag;
    flex-shrink: 0;
  }

  .tab.buttons-always {
    padding-right: 2px;
    transition: background 0.1s, border-color 0.1s;
  }

  .tab.buttons-never {
    transition: background 0.1s, border-color 0.1s;
  }

  .tab.buttons-modifier {
    transition: background 0.1s, border-color 0.1s;
  }

  .tab.buttons-modifier.mod-held:hover {
    padding-right: 2px;
  }

  .tab.unclamped {
    max-width: 50%;
  }

  .tab:hover {
    background: var(--bg-light);
  }

  .tab:not(.buttons-always):not(.buttons-never):not(.buttons-modifier):hover {
    padding-right: 2px;
  }

  .tab.active {
    background: var(--bg-dark);
    border-color: var(--tab-border-active);
  }

  .tab.activity {
    box-shadow: inset 0 -2px 0 var(--tab-border-activity);
  }

  .tab.completed {
    box-shadow: inset 0 -2px 0 var(--green, #9ece6a);
  }

  .tab.failed {
    box-shadow: inset 0 -2px 0 var(--red, #f7768e);
  }

  .tab.tab-alert {
    box-shadow: inset 0 -2px 0 var(--red, #f7768e);
  }

  .tab.tab-question {
    box-shadow: inset 0 -2px 0 var(--yellow, #e0af68);
  }

  .tab.import-highlight {
    box-shadow: inset 0 -2px 0 var(--yellow, #e0af68);
  }

  .tab.dragging {
    opacity: 0.3;
  }

  .tab.drop-before {
    box-shadow: inset 2px 0 0 var(--accent);
  }

  .tab.drop-after {
    box-shadow: inset -2px 0 0 var(--accent);
  }

  /* Foreign tab bar highlighted as a drop target (append to that pane).
     The class is added via classList from the dragging instance, so the
     compiler can't see it — :global() keeps it from being pruned. */
  .tabs-bar:global(.drop-target-bar) {
    box-shadow: inset 0 0 0 2px var(--accent);
  }

  /* Pane-body drop preview (body-appended, like .drag-ghost) — covers the
     half that the new split would occupy, or the whole pane for a move */
  :global(.split-drop-overlay) {
    position: fixed;
    z-index: 999;
    pointer-events: none;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border: 2px solid var(--accent);
    border-radius: 4px;
    transition: left 0.08s ease, top 0.08s ease, width 0.08s ease, height 0.08s ease;
  }

  :global(.drag-ghost) {
    position: fixed;
    pointer-events: none;
    z-index: 10000;
    opacity: 0.5;
    transform: translate(-50%, -50%);
    background: var(--bg-dark);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 5px 10px;
    display: flex;
    align-items: center;
    font-size: 0.923rem;
    color: var(--fg);
    white-space: nowrap;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  }

  :global(.drag-cursor-badge) {
    position: fixed;
    pointer-events: none;
    z-index: 10001;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--green, #9ece6a);
    color: #1a1b26;
    font-size: 1rem;
    font-weight: 800;
    display: flex;
    align-items: center;
    justify-content: center;
    line-height: 1;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.5);
    font-family: -apple-system, system-ui, sans-serif;
  }

  .auto-resume-indicator {
    flex-shrink: 0;
    margin-right: 3px;
    opacity: 0.6;
    line-height: 1;
    display: flex;
    align-items: center;
    transform: rotate(-45deg);
  }

  .agent-bridge-indicator {
    flex-shrink: 0;
    margin-right: 3px;
    line-height: 1;
    display: flex;
    align-items: center;
    font-size: 0.8rem;
    /* Warm/amber, not the blue accent — a bridge is a distinct, attention-worthy state. */
    color: var(--yellow);
    opacity: 0.95;
  }

  .bridge-indicator {
    flex-shrink: 0;
    margin-right: 3px;
    line-height: 1;
    display: flex;
    align-items: center;
  }
  .bridge-connected {
    color: var(--green, #9ece6a);
    opacity: 0.8;
  }
  .bridge-pending {
    color: var(--fg-dim);
    opacity: 0.6;
  }
  .bridge-failed {
    opacity: 0.6;
  }

  .editor-icon {
    flex-shrink: 0;
    margin-right: 4px;
    margin-top: -3px;
    margin-bottom: -3px;
    line-height: 0;
    opacity: 0.7;
  }

  .editor-icon :global(svg) {
    width: 14px;
    height: 14px;
  }

  .editor-icon.editor-dirty {
    color: var(--yellow, #e0af68);
    opacity: 1;
  }


  .indicator {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    margin-right: 4px;
    line-height: 0;
  }

  .completed-indicator {
    color: var(--green, #9ece6a);
  }

  .failed-indicator {
    color: var(--red, #f7768e);
  }

  .claude-active {
    color: var(--accent);
    animation: claude-pulse 1.5s ease-in-out infinite;
  }

  .claude-idle {
    color: var(--green, #9ece6a);
  }

  .claude-permission {
    color: var(--yellow, #e0af68);
  }

  /* Pinned-state glyph in the leading indicator slot — a clickable unpin button. */
  button.indicator.pin-indicator {
    appearance: none;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--accent);
  }

  button.indicator.pin-indicator:hover {
    opacity: 0.65;
  }

  /* Pin button in the hover-action cluster — accent-tinted while the tab is pinned. */
  .pin-action {
    display: flex;
    align-items: center;
  }

  .pin-action.pinned {
    color: var(--accent);
  }

  /* Reset button chrome — this indicator is a clickable reconnect affordance */
  button.indicator.ssh-disconnected {
    appearance: none;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--red, #f7768e);
    animation: ssh-disconnected-pulse 1.8s ease-in-out infinite;
  }

  button.indicator.ssh-disconnected:hover {
    color: var(--yellow, #e0af68);
    animation: none;
  }

  @keyframes ssh-disconnected-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
  }

  @keyframes claude-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.3; transform: scale(0.7); }
  }

  .tab-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.923rem;
  }

  .tab-actions {
    display: flex;
    align-items: center;
    align-self: stretch;
    margin-left: 0;
    opacity: 0;
    width: 0;
    overflow: hidden;
    transition: width 0.15s ease, opacity 0.15s ease, margin-left 0.15s ease;
  }

  .tab:hover .tab-actions {
    opacity: 1;
    width: 44px;
    margin-left: 6px;
  }

  .tab:hover .tab-actions.triple-action {
    width: 66px;
  }

  .tab:hover .tab-actions.quadruple-action {
    width: 88px;
  }

  .tab:hover .tab-actions.quintuple-action {
    width: 110px;
  }

  .tab-actions.always-visible {
    opacity: 1;
    width: 44px;
    margin-left: 6px;
    transition: none;
  }

  .tab-actions.always-visible.triple-action {
    width: 66px;
  }

  .tab-actions.always-visible.quadruple-action {
    width: 88px;
  }

  .tab-actions.always-visible.quintuple-action {
    width: 110px;
  }

  /* modifier mode: suppress normal hover reveal */
  .tab:hover .tab-actions.modifier-only {
    opacity: 0;
    width: 0;
    margin-left: 0;
  }

  /* modifier mode + key held: show on hover like normal */
  .tab:hover .tab-actions.modifier-active {
    opacity: 1;
    width: 44px;
    margin-left: 6px;
  }

  .tab:hover .tab-actions.modifier-active.triple-action {
    width: 66px;
  }

  .tab:hover .tab-actions.modifier-active.quadruple-action {
    width: 88px;
  }

  .tab:hover .tab-actions.modifier-active.quintuple-action {
    width: 110px;
  }

  .tab:hover .tab-actions.never-visible {
    opacity: 0;
    width: 0;
    margin-left: 0;
  }

  .edit-wrapper {
    display: grid;
    align-items: center;
    overflow: hidden;
  }

  .edit-wrapper > * {
    grid-area: 1 / 1;
    font-size: 0.923rem;
    padding: 0 4px;
    font-family: inherit;
  }

  .edit-sizer {
    visibility: hidden;
    white-space: pre;
    min-width: 1ch;
  }

  .edit-input {
    width: 100%;
    min-width: 0;
    padding: 0 4px;
    border: none;
    outline: none;
    background: none;
    color: inherit;
    -webkit-appearance: none;
    appearance: none;
    border-radius: 0;
  }

  .new-tab-btn {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 4px 10px;
    margin-left: 5px;
    border-radius: 4px;
    color: var(--fg-dim);
    -webkit-app-region: no-drag;
  }

  .new-tab-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  /* Shared tab-bar menu trigger buttons (archived tabs, hidden/overflow tabs). */
  .tabbar-menu-wrapper {
    position: relative;
    flex-shrink: 0;
    -webkit-app-region: no-drag;
  }

  .tabbar-menu-btn {
    display: flex;
    align-items: center;
    gap: 3px;
    padding: 4px 8px;
    margin-left: 4px;
    border-radius: 4px;
    color: var(--fg-dim);
    font-size: 0.846rem;
    white-space: nowrap;
    -webkit-app-region: no-drag;
  }

  .tabbar-menu-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .tabbar-menu-btn:hover:not(:disabled) {
    background: var(--bg-light);
    color: var(--fg);
  }

</style>
