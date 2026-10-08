/**
 * Move a workspace, or a single tab, to ANOTHER window with its PTYs still running.
 *
 * All state is one Rust `AppData`, and PTY output is broadcast per PTY id, so the record
 * can simply move between `WindowData`s (`move_workspace_to_window` / `move_tab_to_window`)
 * and the target webview reattach to the live PTYs, as a Reload Window does. What does NOT
 * move by itself is each webview's own memory of those tabs, and that is this module:
 *
 *  - Source: preserve the PTYs (or the unmounting panes kill them), then forget the tabs —
 *    `terminalsStore.unregister` above all, or closing this window later would
 *    `killAllTerminals` a PTY that now belongs to the other one.
 *  - Handed across in the event payload: the SSH MCP bridge (so the target does not type
 *    `export MAITERM_TAB_ID=…` into a live session again) and, for a workspace, its stack
 *    runtime (so running services are not filed `crashed` here and `stopped` there).
 *  - Target: mark the PTYs for reattach, mount every moved tab, then rehydrate the stores
 *    that rebuild from Rust (tasks, agent bridge, mesh).
 *
 * Deliberately NOT carried: the Overlord engine's per-tab memory (escalations, drives) —
 * each window's engine supervises its own tabs and learns a newcomer the way it learns any
 * tab. An agent bridge whose two tabs end up in different windows is disconnected, since
 * a window's `rehydrate` clears a pairing whose partner it cannot see.
 */
import { emitTo, listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { MoveTargetWindow, Tab, Workspace } from '$lib/tauri/types';
import type { MenuItem } from '$lib/components/ContextMenu.svelte';
import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { tasksStore } from '$lib/stores/tasks.svelte';
import { stackStore, type ServiceRuntime } from '$lib/stores/stack.svelte';
import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
import { navHistoryStore } from '$lib/stores/navHistory.svelte';
import { sshDisconnectStore, type DisconnectInfo } from '$lib/stores/sshDisconnect.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { cleanupTab } from '$lib/stores/triggers.svelte';
import { isEditorDirty } from '$lib/stores/editorRegistry.svelte';
import { takeBridgeHandoff, adoptBridgeHandoff, type BridgeHandoff } from '$lib/stores/sshMcpBridge.svelte';

const MOVE_IN_EVENT = 'window-move-in';

interface MoveInPayload {
  kind: 'workspace' | 'tab';
  workspaceId: string;
  tabIds: string[];
  bridges: Record<string, BridgeHandoff>;
  /** A tab's lost-ssh badge lives only in webview memory. */
  disconnects: Record<string, DisconnectInfo>;
  stackRows: Array<[string, ServiceRuntime]>;
  /** A moved tab is followed (target window focused, tab shown) only if the human was
   *  looking at it when it moved. */
  follow?: boolean;
}

interface Released {
  bridges: Record<string, BridgeHandoff>;
  disconnects: Record<string, DisconnectInfo>;
}

const isTerminal = (t: Tab) => t.tab_type === 'terminal' || !t.tab_type;

/** Which tabs can cross windows. A diff tab answers a request held by THIS window's
 *  Claude Code handler, and a board tab is this window's Overlord deck. */
export function canMoveTabAcrossWindows(tab: Tab): boolean {
  return isTerminal(tab) || tab.tab_type === 'editor';
}

function refuse(body: string) {
  toastStore.addToast('Can’t move', body, 'error');
}

/** Everything the source webview must let go of for these tabs, taken BEFORE they leave
 *  its workspace list (their panes unmount on that update). */
function releaseTabs(tabs: Tab[], movingIds: Set<string>): Released {
  const out: Released = { bridges: {}, disconnects: {} };
  for (const tab of tabs) {
    const inst = terminalsStore.get(tab.id);
    if (inst) terminalsStore.preservePty(inst.ptyId);
    const handoff = takeBridgeHandoff(tab.id);
    if (handoff) out.bridges[tab.id] = handoff;
    const lost = sshDisconnectStore.getInfo(tab.id);
    if (lost) out.disconnects[tab.id] = lost;
    const partner = agentBridgeStore.getBridgeInfo(tab.id).partner?.tabId;
    if (partner && movingIds.has(partner)) agentBridgeStore.forgetMovedTab(tab.id);
    else agentBridgeStore.handleTabClosed(tab.id);
    agentMeshStore.handleTabClosed(tab.id);
  }
  return out;
}

/** After the workspace list no longer holds them: drop the instances (whose panes have
 *  just unmounted without killing anything) and the per-tab leftovers. */
function forgetTabs(tabs: Tab[]) {
  for (const tab of tabs) {
    terminalsStore.unregister(tab.id);
    cleanupTab(tab.id);
    sshDisconnectStore.clear(tab.id);
    navHistoryStore.removeTab(tab.id);
  }
}

function dirtyEditor(tabs: Tab[]): Tab | undefined {
  return tabs.find((t) => t.tab_type === 'editor' && isEditorDirty(t.id));
}

export async function moveWorkspaceToWindow(workspaceId: string, targetLabel: string): Promise<void> {
  const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
  if (!ws || ws.overlord) return;
  const tabs = ws.panes.flatMap((p) => p.tabs);
  const dirty = dirtyEditor(tabs);
  if (dirty) return refuse(`“${dirty.name}” has unsaved changes. Save it first.`);

  try {
    await commands.moveWorkspaceToWindow(workspaceId, targetLabel);
  } catch (e) {
    logError(`windowMove: workspace ${workspaceId} → ${targetLabel}: ${e}`);
    return refuse(String(e));
  }

  const movingIds = new Set(tabs.map((t) => t.id));
  const released = releaseTabs(tabs, movingIds);
  const stackRows = stackStore.takeRuntime(workspaceId);
  workspacesStore.applyWindowData(await commands.getWindowData());
  forgetTabs(tabs);
  navHistoryStore.removeWorkspace(workspaceId);
  void tasksStore.rehydrate();

  const payload: MoveInPayload = { kind: 'workspace', workspaceId, tabIds: [...movingIds], ...released, stackRows };
  await emitTo(targetLabel, MOVE_IN_EVENT, payload);
  logInfo(`windowMove: workspace ${workspaceId} (${tabs.length} tabs) → ${targetLabel}`);
}

/** Move a tab to a workspace of any window, this one included (then it is the ordinary
 *  same-window move). It is followed to where it landed only if it was the tab being
 *  viewed; moving a background tab leaves the human where they are. */
export async function moveTabToWindow(
  sourceWsId: string,
  sourcePaneId: string,
  tabId: string,
  targetLabel: string,
  targetWsId: string,
): Promise<void> {
  const sourcePane = workspacesStore.workspaces.find((w) => w.id === sourceWsId)?.panes.find((p) => p.id === sourcePaneId);
  const tab = sourcePane?.tabs.find((t) => t.id === tabId);
  if (!sourcePane || !tab) return;

  const viewing = workspacesStore.activeWorkspaceId === sourceWsId && sourcePane.active_tab_id === tabId;
  const current = getCurrentWindow().label;
  if (targetLabel === current) {
    await workspacesStore.moveTabToWorkspace(sourceWsId, sourcePaneId, tabId, targetWsId);
    if (viewing) await navigateToTab(tabId);
    return;
  }

  if (!canMoveTabAcrossWindows(tab)) return refuse('Only terminal and editor tabs can move to another window.');
  if (dirtyEditor([tab])) return refuse(`“${tab.name}” has unsaved changes. Save it first.`);

  const movedTabWasActive = sourcePane.active_tab_id === tabId;
  const movedTabIndex = sourcePane.tabs.findIndex((t) => t.id === tabId);
  try {
    await commands.moveTabToWindow(sourceWsId, sourcePaneId, tabId, targetLabel, targetWsId);
  } catch (e) {
    logError(`windowMove: tab ${tabId} → ${targetLabel}/${targetWsId}: ${e}`);
    return refuse(String(e));
  }

  const released = releaseTabs([tab], new Set([tabId]));
  await workspacesStore.settleSourceAfterTabLeft(sourceWsId, sourcePaneId, movedTabWasActive, movedTabIndex);
  forgetTabs([tab]);

  const payload: MoveInPayload = { kind: 'tab', workspaceId: targetWsId, tabIds: [tabId], ...released, stackRows: [], follow: viewing };
  await emitTo(targetLabel, MOVE_IN_EVENT, payload);
  logInfo(`windowMove: tab ${tabId} → ${targetLabel}/${targetWsId}`);
}

/** The receiving window. Order matters: reattach marks and bridge state before the tabs
 *  are in the list (their panes read both on mount), auto-start held before the workspace
 *  turns active, stack rows only once the service tabs have registered. */
async function receiveMove(payload: MoveInPayload) {
  const [data, live] = await Promise.all([commands.getWindowData(), commands.listLivePtys()]);
  const liveIds = new Set(live);
  const moving = new Set(payload.tabIds);
  const arrived = data.workspaces.flatMap((w: Workspace) => w.panes.flatMap((p) => p.tabs)).filter((t) => moving.has(t.id));
  const liveTabs = arrived.filter((t) => isTerminal(t) && !!t.pty_id && liveIds.has(t.pty_id));

  terminalsStore.markMovedIn(liveTabs.map((t) => t.pty_id!));
  for (const [tabId, handoff] of Object.entries(payload.bridges)) adoptBridgeHandoff(tabId, handoff);
  for (const [tabId, info] of Object.entries(payload.disconnects)) sshDisconnectStore.mark(tabId, info);
  if (payload.kind === 'workspace') stackStore.holdAutoStart(payload.workspaceId);

  workspacesStore.applyWindowData(data);

  // Mount every moved live tab, not only the visible one: a tab left unmounted here has a
  // pty_id and no instance, which this window reads as SUSPENDED — and resuming it would
  // spawn a second shell beside the running one.
  for (const tab of liveTabs) {
    window.dispatchEvent(new CustomEvent('activate-tab', { detail: tab.id }));
    await terminalsStore.waitForRegister(tab.id, 5000);
  }
  stackStore.adoptRuntime(payload.stackRows);

  void tasksStore.rehydrate();
  agentBridgeStore.rehydrate();
  agentMeshStore.rehydrate();

  if (payload.kind === 'tab' && !payload.follow) return;
  const win = getCurrentWindow();
  await win.unminimize().catch(() => {});
  await win.setFocus().catch(() => {});
  if (payload.kind === 'tab') await navigateToTab(payload.tabIds[0]);
}

/** What a window is called in the menus: its name, else what its titlebar shows. The
 *  ordinal keeps two unnamed windows on same-named workspaces apart. */
function windowLabel(w: MoveTargetWindow, index: number): string {
  if (w.name) return w.name;
  return w.active_workspace_name ? `Window ${index + 1} — ${w.active_workspace_name}` : `Window ${index + 1}`;
}

/** "Move to Window ›" for a workspace: every OTHER window; it lands at the bottom of the
 *  list. `targets` is null while `listMoveTargets` is still in flight. */
export function workspaceMoveItem(targets: MoveTargetWindow[] | null, workspaceId: string): MenuItem {
  const others = (targets ?? []).map((w, i) => ({ w, i })).filter(({ w }) => !w.is_current);
  return {
    label: 'Move to Window',
    action: () => {},
    submenu: others.map(({ w, i }) => ({
      label: windowLabel(w, i),
      action: () => void moveWorkspaceToWindow(workspaceId, w.label),
    })),
  };
}

/** "Move to ›" for a tab: window › workspace, this window first and without the tab's own
 *  workspace. A tab that cannot cross windows is offered this window only. */
export function tabMoveItem(targets: MoveTargetWindow[] | null, tab: Tab, workspaceId: string, paneId: string): MenuItem {
  const crossOk = canMoveTabAcrossWindows(tab);
  const windows = (targets ?? [])
    .map((w, i) => ({ w, i }))
    .filter(({ w }) => w.is_current || crossOk)
    .sort((a, b) => Number(b.w.is_current) - Number(a.w.is_current));
  return {
    label: 'Move to',
    action: () => {},
    submenu: windows.map(({ w, i }) => ({
      label: w.is_current ? 'This window' : windowLabel(w, i),
      action: () => {},
      submenu: w.workspaces
        .filter((ws) => !(w.is_current && ws.id === workspaceId))
        .map((ws) => ({
          label: ws.name,
          action: () => void moveTabToWindow(workspaceId, paneId, tab.id, w.label, ws.id),
        })),
    })),
  };
}

export function listenForWindowMoves(): Promise<UnlistenFn> {
  return listen<MoveInPayload>(MOVE_IN_EVENT, (e) => {
    receiveMove(e.payload).catch((err) => logError(`windowMove: receiving: ${err}`));
  }, { target: getCurrentWindow().label });
}
