/**
 * Agent requests only the human approves (src-tauri/src/agent_requests.rs, docs/agent-requests.md).
 *
 * Three jobs, all in the window that owns the thing an agent wants changed:
 * - the TOOL handlers (moveTab, moveWorkspace, setMesh, linkMeshes, unlinkMesh, createTab,
 *   bridgeWith, disconnectBridge, restartMaiterm): organisation happens at once; anything that changes which agents can message
 *   each other, starting a new agent, and any restart, is filed as a request instead, with the
 *   card's wording built HERE from maiTerm's own state — the agent contributes only its `reason`,
 *   shown as its words;
 * - the EXECUTOR: Rust hands an approved request to the window that filed it, which re-checks
 *   that the world still matches the card and carries it out from the stored arguments;
 * - the OUTCOME: the requesting tab's agent is told what became of it.
 *
 * Nothing here approves anything. `commands.decideAgentRequest` is called only from the card
 * (AgentRequestCard.svelte), on a click.
 */
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { AgentRequest, MoveTargetWindow, Tab, Workspace } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { getAdapter } from '$lib/agents/adapter';
import { overlordStore } from '$lib/stores/overlord.svelte';
import { updaterStore } from '$lib/stores/updater.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { dispatch as notify } from '$lib/stores/notificationDispatch';
import { moveTabToWindow, moveWorkspaceToWindow } from '$lib/stores/windowMove';
import { getDescriptor } from '$lib/agents/descriptor';
import type { AgentRuntime } from '$lib/agents/types';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { isEditorDirty } from '$lib/stores/editorRegistry.svelte';
import { parseSshTarget } from '$lib/utils/sshTarget';

type ToolResult = Record<string, unknown>;

const AWAITING = (r: AgentRequest): ToolResult => ({
  status: 'awaiting_approval',
  request_id: r.id,
  summary: r.summary,
  note: 'The human approves or denies this on a card in maiTerm. You will be told the outcome in this tab; getRequestStatus reads it meanwhile. Do not ask them to approve it in chat: only the card counts.',
});

// ── Reading the layout ──────────────────────────────────────────────────────────────────────

function wsById(id: string): Workspace | undefined {
  return workspacesStore.workspaces.find((w) => w.id === id);
}

function locateTab(tabId: string): { ws: Workspace; paneId: string; tab: Tab } | null {
  for (const ws of workspacesStore.workspaces) {
    for (const p of ws.panes) {
      const tab = p.tabs.find((t) => t.id === tabId);
      if (tab) return { ws, paneId: p.id, tab };
    }
  }
  return null;
}

const agentTabs = (ws: Workspace) => ws.panes.flatMap((p) => p.tabs).filter((t) => !!t.runtime && !t.service_id);
const names = (xs: { name: string }[]) => xs.map((x) => `“${x.name}”`).join(', ');
const count = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`;

/** The mesh a workspace in THIS window puts its tabs in: null when it isn't one. Linked
 *  workspaces share a group (docs/mesh-workspace.md §17), so they share a key. */
function meshKey(ws: { id: string; bridge_all?: boolean; mesh_group?: string | null }): string | null {
  if (!ws.bridge_all) return null;
  return ws.mesh_group ? `group:${ws.mesh_group}` : ws.id;
}

/** The workspaces sharing one mesh with `ws` in this window, itself included. */
function meshOf(ws: Workspace): Workspace[] {
  const key = meshKey(ws);
  return key ? workspacesStore.workspaces.filter((w) => meshKey(w) === key) : [ws];
}

async function moveTargets(): Promise<MoveTargetWindow[]> {
  return commands.listMoveTargets();
}

/** What a card's mesh wording was built from: the workspaces of a mesh and the agent tabs in
 *  them, sorted. Stored with the request and compared before it is carried out, so an approval
 *  covers exactly the agents the card named — an agent could otherwise move more tabs into a
 *  workspace (an ungated move) between the card and the click (review of ffb76a7f). */
function snapshot(wss: Workspace[]): { workspaces: string[]; agents: string[] } {
  return {
    workspaces: wss.map((w) => w.id).sort(),
    agents: wss.flatMap(agentTabs).map((t) => t.id).sort(),
  };
}

/** Field by field: the stored copy has been through Rust's `serde_json::Value`, whose objects
 *  come back with their keys sorted, so the two objects never stringify alike (review of
 *  02a18021). The arrays themselves were sorted when taken. */
function sameSnapshot(stored: unknown, wss: Workspace[]): boolean {
  const s = stored as { workspaces?: unknown; agents?: unknown } | null;
  const now = snapshot(wss);
  const same = (a: unknown, b: string[]) => Array.isArray(a) && a.length === b.length && a.every((x, i) => x === b[i]);
  return !!s && same(s.workspaces, now.workspaces) && same(s.agents, now.agents);
}

const CHANGED = 'the meshes changed after it was asked (workspaces or agents moved); nothing was done, ask again';

// ── Filing ──────────────────────────────────────────────────────────────────────────────────

async function file(
  tabId: string,
  kind: AgentRequest['kind'],
  args: Record<string, unknown>,
  summary: string,
  effects: string[],
  reason: string | undefined,
): Promise<ToolResult> {
  try {
    const r = await commands.fileAgentRequest(tabId, kind, args, summary, effects, reason?.trim() || null);
    announce(r);
    return AWAITING(r);
  } catch (e) {
    return { error: String(e) };
  }
}

/** One request notice per request, in the window that filed it; clicking opens the asking tab's
 *  corner card on it. Dynamic: waiting.svelte imports this store. */
function announce(r: AgentRequest) {
  void notify('An agent asks for your approval', r.summary, 'info', { tabId: r.tab_id }, {
    key: `agent-request:${r.id}`,
    action: () => void import('$lib/stores/waiting.svelte').then(({ waitingStore }) => waitingStore.openOnTab(r.tab_id, r.id)),
  });
}

// ── The tools ───────────────────────────────────────────────────────────────────────────────

async function moveTab(a: { tabId?: string; tab_id?: string; to_workspace_id?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  if (!a.tab_id || !a.to_workspace_id) return { error: 'tab_id and to_workspace_id are required.' };
  const at = locateTab(a.tab_id);
  if (!at) return { error: 'Tab not found.' };
  if (at.tab.service_id) return { error: 'A stack service tab moves with its workspace, not on its own.' };
  if (at.ws.overlord) return { error: 'Tabs in the Overlord workspace stay there.' };
  if (at.ws.id === a.to_workspace_id) return { error: 'The tab is already in that workspace.' };
  const windows = await moveTargets();
  const dest = windows.flatMap((w) => w.workspaces.map((ws) => ({ w, ws }))).find((x) => x.ws.id === a.to_workspace_id);
  if (!dest) return { error: 'Workspace not found (the Overlord workspace takes no tabs).' };
  const here = getCurrentWindow().label;
  const before = meshKey(at.ws);
  const after = meshKey(dest.ws);
  // A mesh is per window: across windows, being in a mesh on either side is a change.
  const changes = dest.w.label === here ? before !== after : before !== null || after !== null;
  const destHere = dest.w.label === here ? wsById(dest.ws.id) : undefined;
  const args = {
    tab_id: a.tab_id,
    from_workspace_id: at.ws.id,
    to_window: dest.w.label,
    to_workspace_id: dest.ws.id,
    from_mesh: snapshot(meshOf(at.ws)),
    to_mesh: destHere ? snapshot(meshOf(destHere)) : null,
  };
  if (!changes) {
    const err = await moveTabToWindow(at.ws.id, at.paneId, a.tab_id, dest.w.label, dest.ws.id, true);
    return err ? { error: err } : { moved: true, tab_id: a.tab_id, workspace: dest.ws.name };
  }
  const effects: string[] = [];
  if (before) {
    const peers = meshOf(at.ws).flatMap(agentTabs).filter((t) => t.id !== a.tab_id);
    effects.push(`It leaves the mesh of ${names(meshOf(at.ws))}: ${count(peers.length, 'agent')} there can no longer message it.`);
  }
  if (after) {
    effects.push(
      dest.w.label === here
        ? `It joins the mesh of ${names(meshOf(wsById(dest.ws.id)!))}: ${count(meshOf(wsById(dest.ws.id)!).flatMap(agentTabs).length, 'agent')} there can message it, and it them.`
        : `It joins the mesh of “${dest.ws.name}” in another window: every agent there can message it, and it them.`,
    );
  }
  return file(a.tabId, 'tab_move', args, `Move “${at.tab.name}” from “${at.ws.name}” to “${dest.ws.name}”`, effects, a.reason);
}

async function moveWorkspace(a: { tabId?: string; workspace_id?: string; to_window_id?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  if (!a.workspace_id || !a.to_window_id) return { error: 'workspace_id and to_window_id are required.' };
  const ws = wsById(a.workspace_id);
  if (!ws) return { error: 'Workspace not found.' };
  if (ws.overlord) return { error: 'The Overlord workspace stays in its window.' };
  const target = (await moveTargets()).find((w) => w.id === a.to_window_id || w.label === a.to_window_id);
  if (!target) return { error: 'Window not found (listWindows gives their ids).' };
  if (target.is_current) return { error: 'The workspace is already in that window.' };
  // A grouped mesh breaks its link here, and rejoins any workspace sharing its group there.
  if (!ws.bridge_all || !ws.mesh_group) {
    const err = await moveWorkspaceToWindow(ws.id, target.label, true);
    return err ? { error: err } : { moved: true, workspace_id: ws.id, window: target.name ?? target.label };
  }
  const linked = meshOf(ws).filter((w) => w.id !== ws.id);
  const effects = [
    linked.length
      ? `It is unlinked from ${names(linked)}: their agents and its ${count(agentTabs(ws).length, 'agent')} can no longer message each other.`
      : 'It carries a mesh link with it, and rejoins any workspace sharing that link in the other window.',
  ];
  const where = target.name ? `“${target.name}”` : 'another window';
  return file(a.tabId, 'workspace_move', { workspace_id: ws.id, to_window: target.label, mesh: snapshot(meshOf(ws)) }, `Move workspace “${ws.name}” to ${where}`, effects, a.reason);
}

async function setMesh(a: { tabId?: string; workspace_id?: string; enabled?: boolean; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  const ws = a.workspace_id ? wsById(a.workspace_id) : undefined;
  if (!ws) return { error: 'Workspace not found.' };
  if (typeof a.enabled !== 'boolean') return { error: 'enabled must be true or false.' };
  if (ws.overlord) return { error: 'The Overlord workspace cannot be a mesh.' };
  if (ws.bridge_all === a.enabled) return { unchanged: true, mesh: a.enabled };
  const agents = agentTabs(ws);
  if (a.enabled) {
    // No snapshot here: the card says agents added later join too, which is what a mesh is.
    return file(a.tabId, 'mesh_enable', { workspace_id: ws.id }, `Make “${ws.name}” a mesh workspace`, [
      agents.length
        ? `Its ${count(agents.length, 'agent')} (${names(agents)}) can message each other, and every agent tab added later joins them.`
        : 'Every agent tab in it, now or later, can message every other.',
    ], a.reason);
  }
  const linked = meshOf(ws).filter((w) => w.id !== ws.id);
  return file(a.tabId, 'mesh_disable', { workspace_id: ws.id, mesh: snapshot(meshOf(ws)) }, `Stop “${ws.name}” being a mesh workspace`, [
    `Its ${count(agents.length, 'agent')} can no longer message each other through the mesh.`,
    ...(linked.length ? [`It is unlinked from ${names(linked)}.`] : []),
  ], a.reason);
}

async function linkMeshes(a: { tabId?: string; workspace_id?: string; with_workspace_id?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  const x = a.workspace_id ? wsById(a.workspace_id) : undefined;
  const y = a.with_workspace_id ? wsById(a.with_workspace_id) : undefined;
  if (!x || !y) return { error: 'Both workspaces must be in the same window (getMeshLayout shows which are).' };
  if (x.id === y.id) return { error: 'A workspace cannot link with itself.' };
  if (x.overlord || y.overlord) return { error: 'The Overlord workspace cannot join a mesh.' };
  const sideX = meshOf(x);
  const sideY = meshOf(y);
  if (sideX.some((w) => w.id === y.id)) return { unchanged: true, note: 'They are already one mesh.' };
  const effects = [
    `The ${count(sideX.flatMap(agentTabs).length, 'agent')} in ${names(sideX)} and the ${count(sideY.flatMap(agentTabs).length, 'agent')} in ${names(sideY)} can message each other.`,
    ...[...sideX, ...sideY].filter((w) => !w.bridge_all).map((w) => `“${w.name}” becomes a mesh workspace.`),
  ];
  return file(
    a.tabId,
    'mesh_link',
    { workspace_id: x.id, with_workspace_id: y.id, side: snapshot(sideX), with_side: snapshot(sideY) },
    `Link ${names(sideX)} with ${names(sideY)} into one mesh`,
    effects,
    a.reason,
  );
}

async function unlinkMesh(a: { tabId?: string; workspace_id?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  const ws = a.workspace_id ? wsById(a.workspace_id) : undefined;
  if (!ws) return { error: 'Workspace not found.' };
  const rest = meshOf(ws).filter((w) => w.id !== ws.id);
  if (!ws.mesh_group || rest.length === 0) return { unchanged: true, note: 'It is not linked with any mesh in this window.' };
  return file(a.tabId, 'mesh_unlink', { workspace_id: ws.id, mesh: snapshot(meshOf(ws)) }, `Unlink “${ws.name}” from ${names(rest)}`, [
    `Its ${count(agentTabs(ws).length, 'agent')} and the ${count(rest.flatMap(agentTabs).length, 'agent')} in ${names(rest)} can no longer message each other. It stays a mesh on its own.`,
  ], a.reason);
}

const RUNTIMES: AgentRuntime[] = ['claude', 'codex', 'gemini'];
const MAX_TAB_NAME = 60;

/** A new agent tab is ALWAYS asked, in a plain workspace too: it is a new agent session, which
 *  could open more of its own, so the card is where the human sees agents being started. */
async function createTab(a: { tabId?: string; workspace_id?: string; cwd?: string; runtime?: string; name?: string; ssh?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  if (!a.workspace_id || !a.cwd) return { error: 'workspace_id and cwd are required.' };
  const ws = wsById(a.workspace_id);
  if (!ws) return { error: 'Workspace not found in this window (getMeshLayout lists them).' };
  if (ws.overlord) return { error: 'The Overlord workspace takes no new tabs.' };
  // Nothing mounts in a suspended workspace, so its agent would not start until it is resumed.
  if (ws.suspended) return { error: `“${ws.name}” is suspended; its tabs start only when the human resumes it.` };
  const runtime = (a.runtime ?? 'claude') as AgentRuntime;
  if (!RUNTIMES.includes(runtime)) return { error: `runtime must be one of ${RUNTIMES.join(', ')}.` };
  const cwd = a.cwd.trim();
  const ssh = a.ssh?.trim() ? parseSshTarget(a.ssh) : null;
  if (a.ssh?.trim() && !ssh) return { error: 'ssh must be [user@]host or an ssh alias, optionally with -p port, -l user, -i keyfile or -J jumphost; nothing after the host.' };
  if (ssh) {
    // Not checkable from here; the shell on the host says so if it is wrong.
    if (!/^(?:\/|~\/|~$)/.test(cwd) || /['\n\r\\]/.test(cwd)) return { error: 'cwd must be an absolute or ~/ path on the host, without quotes.' };
  } else {
    if (!cwd.startsWith('/')) return { error: 'cwd must be an absolute path on this machine.' };
    if (!(await commands.isDirectory(cwd).catch(() => false))) return { error: `${cwd} is not a folder on this machine.` };
  }
  const name = a.name?.trim().slice(0, MAX_TAB_NAME) || null;
  const agent = getDescriptor(runtime).displayName;
  // Only a NAMED agent tab is a mesh member (agentMesh `isAgentMember`).
  const joins = !!name && !!meshKey(ws);
  const effects = [
    ssh
      ? `A new ${agent} session starts on a remote host, through \`${commands.sshCommandLine(ssh)}\`, in ${cwd} there, with its own context and the access that login has there.`
      : `A new ${agent} session starts in ${cwd}, with its own context and the same access to this machine as any tab you open.`,
  ];
  if (joins) {
    effects.push(`It joins the mesh of ${names(meshOf(ws))}: ${count(meshOf(ws).flatMap(agentTabs).length, 'agent')} there can message it, and it them.`);
  }
  return file(
    a.tabId,
    'tab_create',
    { workspace_id: ws.id, cwd, runtime, name, ssh, mesh: joins ? snapshot(meshOf(ws)) : null },
    `Open a new ${agent} tab${name ? ` “${name}”` : ''} in “${ws.name}”${ssh ? `, over ssh to ${ssh.split(' ').pop()}` : ''}`,
    effects,
    a.reason,
  );
}

// ── 1:1 Agent Bridges ───────────────────────────────────────────────────────────────────────
// The other way two agents come to message each other, so asked like a mesh change. On the phone
// there is no picker: the human tells an agent to bridge itself, and approves the card.

/** A tab's LIVE bridge partner (a bridge to a closed tab is no bridge), or null. */
function livePartner(tabId: string): string | null {
  return agentBridgeStore.isBridgedToLivePartner(tabId) ? agentBridgeStore.getPartnerTabId(tabId) : null;
}

const tabName = (tabId: string) => locateTab(tabId)?.tab.name ?? 'a closed tab';

/** What the bridge's disconnect actually does about the other side: it tells it only when it is
 *  not mid-turn (agentBridge `disconnect`), so the card says no more than that. */
const TOLD = (name: string) => `“${name}” is told, unless it is in the middle of a turn at that moment.`;

const BRIDGE_CHANGED = 'the bridges changed after it was asked (one of the tabs was bridged or disconnected meanwhile); nothing was done, ask again';

/** Bridge the CALLER with another tab in this window — the picker's two modes, under its rules:
 *  a tab live-bridged with a third agent is neither connected to nor forked, and only a runtime
 *  that can fork is forked. A bridge the caller already has is closed, and the card says so. */
async function bridgeWith(a: { tabId?: string; with_tab_id?: string; fork?: boolean; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  if (!a.with_tab_id) return { error: 'with_tab_id is required (listWorkspaces gives tab ids).' };
  const me = locateTab(a.tabId);
  if (!me) return { error: 'Your tab is not in this window.' };
  const them = locateTab(a.with_tab_id);
  if (!them) return { error: 'That tab is not in your window. A bridge joins two tabs in one window (getMeshLayout / listWorkspaces show where each is).' };
  if (them.tab.id === me.tab.id) return { error: 'You cannot bridge with yourself.' };
  if ((them.tab.tab_type ?? 'terminal') !== 'terminal' || them.tab.service_id) return { error: `“${them.tab.name}” is not an agent tab.` };
  if (!claudeStateStore.getState(them.tab.id)) return { error: `“${them.tab.name}” has no running agent session right now.` };
  const fork = a.fork === true;
  const mine = livePartner(me.tab.id);
  const theirs = livePartner(them.tab.id);
  if (!fork && mine === them.tab.id) return { unchanged: true, note: `You are already bridged with “${them.tab.name}”.` };
  if (fork && theirs) return { error: `“${them.tab.name}” is in a bridge already, so it is not forked (the human's picker has the same rule).` };
  if (!fork && theirs) return { error: `“${them.tab.name}” is bridged with “${tabName(theirs)}”; it would have to disconnect first.` };
  if (fork && !getAdapter(workspacesStore.getTabRuntime(them.tab.id)).supportsFork) return { error: `“${them.tab.name}” runs an agent that cannot be forked; bridge with it without fork.` };
  const closes = mine ? [`“${me.tab.name}”’s bridge with “${tabName(mine)}” is closed; ${TOLD(tabName(mine))}`] : [];
  const where = them.ws.id === me.ws.id ? '' : ` (in “${them.ws.name}”)`;
  const agent = getDescriptor(workspacesStore.getTabRuntime(them.tab.id)).displayName;
  // No agent-written `purpose`: the bridge's opener presents it as the human operator's words,
  // and the card would not show it (review of c9d2419c). The agent's `reason` is on the card.
  const args = { caller_tab_id: me.tab.id, target_tab_id: them.tab.id, caller_partner: mine };
  if (fork) {
    // Where the copy RUNS is the card's to say: forking an agent that is over ssh starts the copy
    // on that host (review of c9d2419c). Read now, compared again on approval.
    // Where it runs is read off its terminal; a tab not loaded in this window since maiTerm
    // started has none yet, and "local" would be a guess (review of 3302b9e9).
    if (!terminalsStore.get(them.tab.id)) return { error: `“${them.tab.name}” is not loaded in this window yet, so where its agent runs cannot be read. Ask the human to open its tab once, then ask again.` };
    const ft = await agentBridgeStore.forkTarget(them.tab.id);
    if ('error' in ft) return { error: ft.error };
    const ssh = ft.sshCommand ?? null;
    // This card goes to the phone. getPtyInfo cleans maiTerm's export (MAITERM_AUTH) off the
    // line, a full-path ssh or autossh included; anything the cleaner still does not recognise
    // would come back raw, so a line still carrying it is refused rather than shown.
    if (ssh && /MAITERM_|\bexec\s+\$?SHELL\b/.test(ssh)) {
      return { error: `“${them.tab.name}”'s ssh command cannot be read safely, so it is not forked by request. The human can fork it with the Agent Bridge picker.` };
    }
    const runs = ssh
      ? `on the remote host “${them.tab.name}” runs on, through \`${commands.sshCommandLine(ssh)}\`, with the access that login has there`
      : 'on this machine, with the same access as any tab you open';
    return file(a.tabId, 'bridge_fork', { ...args, ssh_command: ssh }, `Fork “${them.tab.name}”${where} into a new tab and bridge it with “${me.tab.name}”`, [
      `A new ${agent} session starts in a new pane beside “${me.tab.name}” with a copy of “${them.tab.name}”’s conversation so far (the original is untouched). It runs ${runs}.`,
      `“${me.tab.name}” and the copy can message each other directly over a 1:1 bridge.`,
      ...closes,
    ], a.reason);
  }
  return file(a.tabId, 'bridge_connect', args, `Bridge “${me.tab.name}” with “${them.tab.name}”${where}`, [
    `The two agents can message each other directly over a 1:1 bridge, and each is told so.`,
    ...closes,
  ], a.reason);
}

/** Close the CALLER's bridge. A bridge to a closed tab carries no messages, so it goes at once. */
async function disconnectBridge(a: { tabId?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  if (!locateTab(a.tabId)) return { error: 'Your tab is not in this window.' };
  if (!agentBridgeStore.isBridged(a.tabId)) return { unchanged: true, note: 'You are not bridged with any agent.' };
  const partner = livePartner(a.tabId);
  if (!partner) {
    agentBridgeStore.disconnect(a.tabId);
    return { disconnected: true, note: 'Your bridge was to a tab that is gone; it is closed.' };
  }
  const me = tabName(a.tabId);
  const them = tabName(partner);
  return file(a.tabId, 'bridge_disconnect', { caller_tab_id: a.tabId, partner_tab_id: partner }, `Close the bridge between “${me}” and “${them}”`, [
    `They can no longer message each other directly; ${TOLD(them)} A mesh they share is not affected.`,
  ], a.reason);
}

async function restartMaiterm(a: { tabId?: string; update?: boolean; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  if (typeof a.update !== 'boolean') return { error: 'update must be true or false.' };
  const effects = [
    'Every maiTerm window closes and reopens. Agents come back through their auto-resume; anything one was in the middle of is interrupted.',
    'Unsaved changes in editor tabs are lost.',
  ];
  if (!a.update) return file(a.tabId, 'restart', { update: false }, 'Restart maiTerm', effects, a.reason);
  if (!updaterStore.installed) {
    const u = updaterStore.currentUpdate ?? (await updaterStore.checkForUpdates(true));
    if (!u) return { error: 'There is no maiTerm update to install. Pass update:false to only restart.' };
  }
  const version = updaterStore.version;
  return file(a.tabId, 'restart', { update: true, version }, `Install maiTerm ${version ? `v${version} ` : 'update '}and restart`, effects, a.reason);
}

// ── Reloads ─────────────────────────────────────────────────────────────────────────────────

/** Whether a reloaded tab's agent comes back: the replacement types the tab's auto-resume
 *  command on mount (TerminalPane), and nothing else restarts it. */
const resumesOnReload = (t: Tab) => t.auto_resume_enabled !== false && !!t.auto_resume_command;

/** Reload a terminal tab as Cmd+Shift+R does (a replacement under a new id). The caller's OWN tab
 *  goes at once — it is the one interrupted — but only when it will come back; anyone else's
 *  interrupts another agent, so it is asked. */
async function reloadTab(a: { tabId?: string; tab_id?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  const target = a.tab_id || a.tabId;
  const own = target === a.tabId;
  const at = locateTab(target);
  if (!at) return { error: own ? 'Your tab is not in this window.' : 'Tab not found (listWorkspaces gives tab ids).' };
  if (at.tab.service_id) return { error: `“${at.tab.name}” runs a stack service; use restartService.` };
  if ((at.tab.tab_type ?? 'terminal') !== 'terminal') return { error: `“${at.tab.name}” is not a terminal tab.` };
  // A suspended or never-opened tab has no shell to restart; reloading it would spawn one nobody asked for.
  if (!terminalsStore.get(target)) return { error: `“${at.tab.name}” has no running terminal (it is suspended or not opened yet), so there is nothing to reload.` };
  const resumes = resumesOnReload(at.tab);
  if (own) {
    if (!resumes) return { error: 'Your tab has no auto-resume, so a reload would end your session for good. Turn it on with setAutoResume first.' };
    // Requests are keyed by the asking tab's id, which a reload replaces: the outcome of one still
    // open would reach no one, and getRequestStatus from the new id would not find it.
    const open = (await commands.listAgentRequests().catch(() => [])).filter((r) => r.tab_id === target && (r.status === 'pending' || r.status === 'approved'));
    if (open.length) return { error: `You have ${count(open.length, 'request')} waiting on the human (${open.map((r) => r.summary).join('; ')}). A reload gives your tab a new id, and you would not be told the outcome; reload once they are answered.` };
    // A reload kills the box with the shell: never take a human's draft with it.
    const hasDraft = async () => (await commands.agentInputBox(target).catch(() => 'unknown')) === 'has_text';
    if (await hasDraft()) return { error: 'The human has a draft typed in your input box; a reload would lose it. Not reloaded.' };
    // After this answer is on its way: the reload kills the shell this call came from.
    setTimeout(async () => {
      const now = locateTab(target);
      if (!now) return;
      if (await hasDraft()) {
        logInfo(`agentRequests: own reload of ${target.slice(0, 8)} dropped, a draft appeared in its input box`);
        return;
      }
      workspacesStore.reloadTab(now.ws.id, now.paneId, target)
        .catch((e) => logError(`agentRequests: reloading own tab ${target.slice(0, 8)} failed: ${e}`));
    }, 1500);
    return { reloading: true, note: 'Your tab reloads in a moment under a new tab id, and its auto-resume brings you back with this conversation. Do not start anything else now: it would be interrupted.' };
  }
  const effects = [
    resumes
      ? `Its shell restarts under a new tab id in the same place, keeping its name, notes and tasks. Whatever runs there now is interrupted; its agent comes back through the tab's auto-resume.`
      : `Its shell restarts under a new tab id in the same place, keeping its name, notes and tasks. It has no auto-resume: whatever runs there now is stopped and nothing restarts it.`,
  ];
  return file(a.tabId, 'tab_reload', { tab_id: target, resumes }, `Reload “${at.tab.name}” in “${at.ws.name}”`, effects, a.reason);
}

/** Reload a window's webview, as Reload Current Window does. Always asked: PTYs live on, but what
 *  only the interface holds (unsaved editors, open panels and dialogs) does not. */
async function reloadWindow(a: { tabId?: string; window_id?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  const windows = await moveTargets();
  const target = a.window_id ? windows.find((w) => w.id === a.window_id || w.label === a.window_id) : windows.find((w) => w.is_current);
  if (!target) return { error: 'Window not found (listWindows gives their ids).' };
  const shown = target.name ?? target.active_workspace_name ?? target.label;
  const effects = [
    'Its shells and agents keep running, and every tab reattaches once the window is back.',
  ];
  let dirty: string[] | null = null;
  if (target.is_current) {
    const tabs = dirtyEditors();
    dirty = tabs.map((t) => t.id);
    effects.push(tabs.length ? `Unsaved changes in ${count(tabs.length, 'editor tab')} are lost: ${names(tabs)}.` : 'No editor tab in it has unsaved changes.');
  } else {
    effects.push('Unsaved changes in its editor tabs are lost.');
  }
  return file(a.tabId, 'window_reload', { window: target.label, dirty }, `Reload the maiTerm window ${target.name ? `“${target.name}”` : `showing “${shown}”`}`, effects, a.reason);
}

/** This window's editor tabs with unsaved changes, sorted by id (compared on approval). */
function dirtyEditors(): Tab[] {
  return workspacesStore.workspaces.flatMap((w) => w.panes.flatMap((p) => p.tabs)).filter((t) => isEditorDirty(t.id)).sort((x, y) => x.id.localeCompare(y.id));
}

// ── MCP servers through maiTerm (docs/mcp-proxy.md §7) ──────────────────────────────────────

/** A path segment that reads as a key rather than a word: long, and letters mixed with digits
 *  (Zapier's `/api/mcp/s/<secret>/mcp`). Such URLs ARE the credential. */
const keyLike = (seg: string) =>
  seg.length >= 32 || (seg.length >= 16 && /^[A-Za-z0-9_-]+$/.test(seg) && /\d/.test(seg) && /[A-Za-z]/.test(seg));

/** A URL as an agent may see it: no query, no fragment, key-like segments masked. A server the
 *  human added by hand may carry its key in the URL, and this list reaches every agent. */
function shownUrl(raw: string): string {
  try {
    const u = new URL(raw);
    const path = u.pathname.split('/').map((s) => (keyLike(s) ? '…' : s)).join('/');
    return `${u.origin}${path}${u.search ? '?…' : ''}`;
  } catch {
    return '(unreadable URL)';
  }
}

/** The servers maiTerm manages, as an agent may read them. Never a credential (the view has none). */
async function listMcpServers(): Promise<ToolResult> {
  const servers = await commands.mcpServersList();
  return {
    servers: servers.map((s) => ({
      name: s.entry_name,
      url: shownUrl(s.url),
      status: s.status.state,
      ...(s.status.detail ? { detail: s.status.detail } : {}),
      tools: s.tools,
      workspaces: s.workspaces.length === 0 ? 'all' : s.workspaces.map((id) => wsById(id)?.name ?? id),
      ...(s.duplicates.length ? { also_installed_directly: s.duplicates } : {}),
    })),
    note: servers.length
      ? 'Each appears to agents as its own MCP server under `name`. A server in needs_sign_in or unreachable keeps its tools listed; calls fail with a sentence to pass on to the human.'
      : 'None yet. addMcpServer proposes one; the human approves it.',
  };
}

/** A server name from the URL's host, as the Preferences pane suggests one. */
function nameFromUrl(u: URL): string {
  const labels = u.hostname.split('.').filter((l) => !['mcp', 'api', 'www'].includes(l));
  const pick = labels.length > 1 ? labels[labels.length - 2] : labels[0] ?? '';
  return pick.toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/^-+/, '').slice(0, 40);
}

async function addMcpServer(a: { tabId?: string; url?: string; name?: string; reason?: string }): Promise<ToolResult> {
  if (!a.tabId) return { error: 'No tab identity: run /maiterm init.' };
  let url: URL;
  try {
    url = new URL((a.url ?? '').trim());
  } catch {
    return { error: 'url must be the server\'s https endpoint, e.g. https://mcp.linear.app/mcp.' };
  }
  // A key in the URL would go onto the card, into the request, into maiTerm's plaintext config
  // and into listMcpServers for every agent. So any sign of one is refused, and the human adds
  // such a server themselves.
  if (
    url.username || url.password || url.hash
    || [...url.searchParams.keys()].some((k) => /key|token|secret|auth|password|sig/i.test(k))
    || url.pathname.split('/').some(keyLike)
  ) {
    return { error: 'The URL looks like it carries a key or token. Don’t propose it: tell the human they can add it themselves in maiTerm → Preferences → MCP servers, or install it directly.' };
  }
  if (url.search) return { error: 'Propose the plain endpoint, without a query string.' };
  const name = (a.name ?? '').trim() || nameFromUrl(url);
  if (!/^[a-z0-9][a-z0-9_-]{0,39}$/.test(name)) return { error: `"${name}" can't be a server name: 1–40 lowercase letters, digits, - or _.` };
  const existing = await commands.mcpServersList();
  if (existing.some((s) => s.id === name)) return { error: `maiTerm already has an MCP server named ${name}. listMcpServers shows it.` };
  // A name the human's own entry holds is never written over, so the server would quietly not
  // reach that runtime: ask for another name instead.
  const shownName = import.meta.env.DEV ? `${name}-dev` : name;
  const native = await commands.mcpNativeNames().catch(() => [] as string[]);
  if (native.includes(shownName)) {
    return { error: `The human already has an MCP server of their own named “${shownName}”. Pass a different name.` };
  }
  if (existing.some((s) => s.url.replace(/\/+$/, '') === url.href.replace(/\/+$/, ''))) {
    return { error: 'maiTerm already serves that URL. listMcpServers shows it.' };
  }
  let probe: commands.McpProbe;
  try {
    probe = await commands.mcpServerProbe(url.href);
  } catch (e) {
    return { error: `maiTerm checked ${url.href} and can't add it: ${e}` };
  }
  if (probe.auth === 'oauth' && !probe.can_register) {
    // It takes a client id or a token only the human has: the add form is where that goes.
    return {
      error: `${url.host} doesn't let apps register themselves, so it needs a client id or a token that only the human can supply. Tell them they can add it in maiTerm → Preferences → MCP servers (or install it directly).`,
    };
  }
  let issuerHost = url.host;
  try {
    if (probe.issuer) issuerHost = new URL(probe.issuer).host;
  } catch { /* keep the server's host */ }
  // The name agents see: a dev build suffixes it, as maiterm-dev (mcp_proxy::entries::entry_name).
  const shown = shownName;
  const signIn =
    probe.auth === 'oauth'
      ? `It needs you to sign in with ${issuerHost}. Approving here opens that in your browser; approving from your phone leaves it waiting in Preferences → MCP servers on this computer.`
      : probe.auth === 'token'
        ? 'It needs a token from the service, which you paste in Preferences → MCP servers. Until then its tools fail.'
        : 'It needs no sign-in.';
  // Only the runtimes whose integration is on get an entry (mcp_proxy::entries::sync, and the ssh
  // bridge's remote merge), so the card names just those: it must be true about who gets it.
  const p = preferencesStore;
  const runtimes = [
    ...(p.claudeCodeIde ? ['Claude'] : []),
    ...(p.codexIde ? ['Codex'] : []),
  ];
  const remote = p.claudeCodeIde && p.claudeCodeIdeSsh;
  const who = runtimes.length
    ? `${runtimes.join(' and ')} here${remote ? ', and Claude on the ssh hosts you have bridged,' : ''}`
    : '';
  const effects = [
    who
      ? `${who} ${runtimes.length > 1 || remote ? 'get' : 'gets'} its tools as an MCP server named “${shown}”, in every workspace. Its tools load into every agent session started after this; one already running doesn't see it until it restarts.`
      : `No agent gets its tools until you turn on the Claude Code or Codex integration in Preferences; then they appear as an MCP server named “${shown}”, in every workspace.`,
    signIn,
    ...(probe.duplicates.length ? [`You also have it installed directly (${probe.duplicates.join('; ')}): agents would get every tool twice until you remove one (“Move to maiTerm” in Preferences).`] : []),
  ];
  return file(a.tabId, 'mcp_add', { name, url: url.href, auth: probe.auth }, `Add the MCP server “${shown}” (${url.origin}${url.pathname}) through maiTerm`, effects, a.reason);
}

// ── Carrying out an approved request ────────────────────────────────────────────────────────

/** Re-check that the world still matches what the human approved, then do it. Resolves the
 *  outcome line, or throws with why it could not be done. */
async function execute(r: AgentRequest): Promise<string> {
  const s = (k: string) => String(r.args[k] ?? '');
  switch (r.kind) {
    case 'mesh_enable':
    case 'mesh_disable': {
      const ws = wsById(s('workspace_id'));
      if (!ws) throw new Error('the workspace is gone');
      const on = r.kind === 'mesh_enable';
      if (!on && ws.bridge_all && !sameSnapshot(r.args.mesh, meshOf(ws))) throw new Error(CHANGED);
      if (ws.bridge_all !== on) await agentMeshStore.setMeshEnabled(ws.id, on);
      return on ? `“${ws.name}” is a mesh workspace` : `“${ws.name}” is no longer a mesh`;
    }
    case 'mesh_link': {
      const x = wsById(s('workspace_id'));
      const y = wsById(s('with_workspace_id'));
      if (!x || !y) throw new Error('a workspace is gone');
      if (!sameSnapshot(r.args.side, meshOf(x)) || !sameSnapshot(r.args.with_side, meshOf(y))) throw new Error(CHANGED);
      const res = await agentMeshStore.linkMeshes(x.id, y.id);
      if ('error' in res) throw new Error(res.error);
      return 'linked';
    }
    case 'mesh_unlink': {
      const ws = wsById(s('workspace_id'));
      if (!ws) throw new Error('the workspace is gone');
      if (!sameSnapshot(r.args.mesh, meshOf(ws))) throw new Error(CHANGED);
      await agentMeshStore.unlinkMesh(ws.id);
      return `“${ws.name}” is unlinked`;
    }
    case 'tab_move': {
      const at = locateTab(s('tab_id'));
      if (!at || at.ws.id !== s('from_workspace_id')) throw new Error('the tab is no longer where it was when you approved');
      // The card named the agents on both sides; either mesh having changed is a different question.
      const dest = r.args.to_mesh ? wsById(s('to_workspace_id')) : undefined;
      if (!sameSnapshot(r.args.from_mesh, meshOf(at.ws)) || (r.args.to_mesh && (!dest || !sameSnapshot(r.args.to_mesh, meshOf(dest))))) {
        throw new Error(CHANGED);
      }
      const err = await moveTabToWindow(at.ws.id, at.paneId, at.tab.id, s('to_window'), s('to_workspace_id'), true);
      if (err) throw new Error(err);
      return 'moved';
    }
    case 'workspace_move': {
      const ws = wsById(s('workspace_id'));
      if (!ws) throw new Error('the workspace is gone');
      if (!sameSnapshot(r.args.mesh, meshOf(ws))) throw new Error(CHANGED);
      const err = await moveWorkspaceToWindow(ws.id, s('to_window'), true);
      if (err) throw new Error(err);
      return 'moved';
    }
    case 'tab_create': {
      const ws = wsById(s('workspace_id'));
      if (!ws) throw new Error('the workspace is gone');
      if (ws.suspended) throw new Error(`“${ws.name}” was suspended after it was asked; nothing was opened`);
      const name = (r.args.name as string | null) ?? null;
      // The card said whether it joins a mesh, and whose; either having changed is a different question.
      const joins = !!name && !!meshKey(ws);
      if (joins !== !!r.args.mesh || (joins && !sameSnapshot(r.args.mesh, meshOf(ws)))) throw new Error(CHANGED);
      const ssh = (r.args.ssh as string | null) ?? null;
      if (!ssh && !(await commands.isDirectory(s('cwd')).catch(() => false))) throw new Error(`${s('cwd')} is no longer a folder`);
      const id = await workspacesStore.openAgentTab(ws.id, s('cwd'), s('runtime') as AgentRuntime, name, ssh);
      if (!id) throw new Error('the workspace is gone');
      return `opened tab ${id}${name ? ` (“${name}”)` : ''} in “${ws.name}”; its agent is starting`;
    }
    case 'bridge_connect':
    case 'bridge_fork': {
      const me = s('caller_tab_id');
      const them = s('target_tab_id');
      if (!locateTab(me)) throw new Error('your tab is gone');
      const target = locateTab(them);
      if (!target) throw new Error('the other tab is gone');
      // The card said which bridge of the caller's closes, and that the other side was free.
      if (livePartner(me) !== ((r.args.caller_partner as string | null) ?? null)) throw new Error(BRIDGE_CHANGED);
      const theirs = livePartner(them);
      if (theirs && theirs !== me) throw new Error(BRIDGE_CHANGED);
      if (r.kind === 'bridge_connect') {
        const res = await agentBridgeStore.bridgeExistingTab(me, them);
        if (!res.ok) throw new Error(res.error);
        return `bridged with “${target.tab.name}”`;
      }
      if (theirs) throw new Error(BRIDGE_CHANGED);
      if (!terminalsStore.get(them)) throw new Error(`“${target.tab.name}” is no longer loaded in this window; nothing was done, ask again`);
      const ft = await agentBridgeStore.forkTarget(them);
      if ('error' in ft) throw new Error(ft.error);
      // The card said where the copy runs: local, or through this exact ssh command.
      if ((ft.sshCommand ?? null) !== ((r.args.ssh_command as string | null) ?? null)) {
        // Not echoing the new command: it is unchecked, and this outcome reaches the phone too.
        throw new Error(`“${target.tab.name}” is no longer where the card said (${ft.sshCommand ? 'now over a different ssh connection' : 'now local'}); nothing was done, ask again`);
      }
      const old = (r.args.caller_partner as string | null) ?? null;
      if (old) agentBridgeStore.disconnect(me); // the card said so; establishBridge refuses a live one
      const res = await agentBridgeStore.establishBridge(me, ft);
      if (!res.ok) {
        // Not "nothing was done": the bridge the card named as closing is already gone.
        throw new Error(old ? `your bridge with “${tabName(old)}” was closed, but the fork failed: ${res.error}` : res.error);
      }
      return `forked “${target.tab.name}” into tab ${res.partnerTabId} and bridged it with you; the copy is starting`;
    }
    case 'bridge_disconnect': {
      const me = s('caller_tab_id');
      if (!locateTab(me)) throw new Error('your tab is gone');
      if (livePartner(me) !== s('partner_tab_id')) throw new Error(BRIDGE_CHANGED);
      agentBridgeStore.disconnect(me);
      return 'the bridge is closed';
    }
    case 'mcp_add': {
      const auth = s('auth');
      // add() re-checks the name and URL, and probes again for a sign-in.
      const v = await commands.mcpServerAdd(s('name'), s('url'), auth, { requestId: r.id });
      const as = `added as the MCP server “${v.entry_name}”`;
      if (auth === 'oauth') {
        // From the phone the human isn't at this Mac: a browser opening here would wait unseen.
        if (r.decided_by === 'phone') return `${as}; it waits for the human to sign in on their Mac (Preferences → MCP servers). Its tools fail until then`;
        try {
          await shellOpen(await commands.mcpServerSignIn(v.id));
          return `${as}; the human is signing in in their browser. Its tools fail until they finish`;
        } catch (e) {
          return `${as}, but the sign-in didn't start (${e}); the human can sign in from Preferences → MCP servers`;
        }
      }
      if (auth === 'token') return `${as}; it waits for the human to paste a token in Preferences → MCP servers. Its tools fail until then`;
      return `${as}. Agent sessions started from now on get its tools, here and on the ssh hosts bridged now; one already running (yours included) doesn't see a new server until it restarts`;
    }
    case 'restart': {
      if (r.args.update) {
        if (!updaterStore.installed) {
          if (!updaterStore.currentUpdate) await updaterStore.checkForUpdates(true);
          // Approved for one version; a different one is a different question.
          if (updaterStore.version !== (r.args.version ?? null)) throw new Error(`the update on offer is now v${updaterStore.version ?? 'none'}, not what you approved`);
          if (!(await updaterStore.downloadAndInstall())) throw new Error('the update did not install (see the log)');
        }
      }
      await commands.settleAgentRequest(r.id, true, 'restarting').catch(() => {});
      await updaterStore.restart();
      throw new Error('the relaunch failed (see the log)');
    }
    case 'tab_reload': {
      const at = locateTab(s('tab_id'));
      if (!at) throw new Error('the tab is gone');
      if (!terminalsStore.get(at.tab.id)) throw new Error(`“${at.tab.name}” is no longer running; nothing was done`);
      // The card said whether its agent comes back.
      if (resumesOnReload(at.tab) !== r.args.resumes) throw new Error(`“${at.tab.name}”’s auto-resume changed after it was asked; nothing was done, ask again`);
      const id = await workspacesStore.reloadTab(at.ws.id, at.paneId, at.tab.id);
      if (!id) throw new Error(`“${at.tab.name}” could not be reloaded`);
      return `reloaded; “${at.tab.name}” is now tab ${id}`;
    }
    case 'window_reload': {
      const label = s('window');
      if (label !== getCurrentWindow().label) {
        await commands.reloadWindow(label);
        return 'the window is reloading';
      }
      // The card counted this window's unsaved editors; an edit made while it waited is a different question.
      const was = (r.args.dirty as string[] | null) ?? [];
      const now = dirtyEditors().map((t) => t.id);
      if (now.length !== was.length || now.some((id, i) => id !== was[i])) {
        throw new Error('editor tabs in this window were edited or saved after it was asked, so the card no longer says what would be lost; nothing was done, ask again');
      }
      // This window goes with it: settle first, as a restart does.
      await commands.settleAgentRequest(r.id, true, 'the window is reloading').catch(() => {});
      window.location.reload();
      return 'the window is reloading';
    }
  }
}

/** Approved requests run one at a time: two approved together (a tab reload and a new tab in the
 *  same pane) each rewrite the pane's tab list from their own read of it. */
let executing: Promise<void> = Promise.resolve();

function onApproved(r: AgentRequest): Promise<void> {
  executing = executing.then(() => carryOut(r));
  return executing;
}

async function carryOut(r: AgentRequest) {
  logInfo(`agentRequests: carrying out ${r.kind} ${r.id}`);
  let ok = false;
  let outcome: string;
  try {
    outcome = await execute(r);
    ok = true;
  } catch (e) {
    outcome = e instanceof Error ? e.message : String(e);
    logError(`agentRequests: ${r.kind} ${r.id} failed: ${outcome}`);
  }
  await commands.settleAgentRequest(r.id, ok, outcome).catch((e) => logError(`agentRequests: settle ${r.id}: ${e}`));
  if (!ok) toastStore.addToast('Approved, but it didn’t work', `${r.summary}: ${outcome}`, 'error');
}

/** Tell the agent that asked. Only the window holding its terminal can. */
async function onSettled(r: AgentRequest) {
  toastStore.removeByKey(`agent-request:${r.id}`);
  if (!terminalsStore.get(r.tab_id)) {
    // A tab moving here (an approved cross-window move of the asking tab itself) can be in
    // neither window when the settle arrives: gone from the old one, not yet in this one's list.
    // So every window without its terminal waits a little, and whichever registers it says it;
    // the rest time out (bounded, one timer each).
    await terminalsStore.waitForRegister(r.tab_id, 10_000);
    if (!terminalsStore.get(r.tab_id)) return;
  }
  // The restart is its own outcome; a notice typed now would race the shutdown.
  if (r.kind === 'restart' && r.status === 'done') return;
  // So is a reload of the window the asker is in.
  if (r.kind === 'window_reload' && r.status === 'done' && r.args.window === r.window_label) return;
  const by = r.decided_by === 'phone' ? ' (from their phone)' : '';
  const text =
    r.status === 'done'
      ? `[maiTerm] The human approved your request${by} — ${r.summary} — and it is done: ${r.outcome ?? ''}.`
      : r.status === 'failed'
        ? `[maiTerm] The human approved your request${by} — ${r.summary} — but it could not be done: ${r.outcome ?? 'unknown error'}.`
        : r.status === 'denied'
          ? `[maiTerm] The human denied your request${by}: ${r.summary}. Do not ask again unless they bring it up.`
          : r.status === 'expired'
            ? `[maiTerm] Your request lapsed after 30 minutes without an answer from the human: ${r.summary}. Nothing was done.`
            : null;
  if (text && !(await overlordStore.tellTab(r.tab_id, text, 'agent-request'))) {
    logInfo(`agentRequests: could not tell ${r.tab_id.slice(0, 8)} about ${r.id} (${r.status}); getRequestStatus has it`);
  }
}

// ── The list the cards draw ─────────────────────────────────────────────────────────────────

function createAgentRequestsStore() {
  let requests = $state<AgentRequest[]>([]);
  let unlisten: UnlistenFn[] = [];
  /** Rust lapses a request only when the list is read. maiLink reads it every few seconds, but
   *  maiLink is off by default, and then a lapsed card stayed up and its agent was never told
   *  (review of 3973dc21). This read is the clock. */
  let sweepTimer: ReturnType<typeof setInterval> | undefined;

  async function refresh() {
    requests = await commands.listAgentRequests().catch(() => requests);
  }

  return {
    /** Waiting for the human, oldest first — this window's only: windows are isolated, and the
     *  window that filed a request is the one that carries it out. Rust's list is app-wide (the
     *  phone sees them all). */
    get pending(): AgentRequest[] {
      const me = getCurrentWindow().label;
      return requests.filter((r) => r.status === 'pending' && r.window_label === me);
    },
    /** The human's click: the ONLY caller of decideAgentRequest. */
    async decide(id: string, approve: boolean): Promise<AgentRequest | null> {
      const r = await commands.decideAgentRequest(id, approve);
      await refresh();
      return r;
    },
    moveTab,
    moveWorkspace,
    setMesh,
    linkMeshes,
    unlinkMesh,
    createTab,
    bridgeWith,
    disconnectBridge,
    restartMaiterm,
    reloadTab,
    reloadWindow,
    listMcpServers,
    addMcpServer,
    async init() {
      const me = getCurrentWindow().label;
      unlisten = await Promise.all([
        listen('agent-requests-changed', () => void refresh()),
        // Emitted to the filing window only (Rust `emit_to`); checked again in case.
        listen<AgentRequest>('agent-request-approved', (e) => {
          if (e.payload.window_label === me) void onApproved(e.payload);
        }),
        listen<AgentRequest>('agent-request-settled', (e) => void onSettled(e.payload)),
      ]);
      await refresh();
      sweepTimer = setInterval(() => void refresh(), 30_000);
    },
    destroy() {
      unlisten.forEach((u) => u());
      unlisten = [];
      clearInterval(sweepTimer);
    },
  };
}

export const agentRequestsStore = createAgentRequestsStore();
