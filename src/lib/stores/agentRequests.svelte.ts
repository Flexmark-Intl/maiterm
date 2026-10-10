/**
 * Agent requests only the human approves (src-tauri/src/agent_requests.rs, docs/agent-requests.md).
 *
 * Three jobs, all in the window that owns the thing an agent wants changed:
 * - the TOOL handlers (moveTab, moveWorkspace, setMesh, linkMeshes, unlinkMesh, createTab,
 *   restartMaiterm): organisation happens at once; anything that changes which agents can message
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
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { AgentRequest, MoveTargetWindow, Tab, Workspace } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
import { overlordStore } from '$lib/stores/overlord.svelte';
import { updaterStore } from '$lib/stores/updater.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { dispatch as notify } from '$lib/stores/notificationDispatch';
import { moveTabToWindow, moveWorkspaceToWindow } from '$lib/stores/windowMove';
import { getDescriptor } from '$lib/agents/descriptor';
import type { AgentRuntime } from '$lib/agents/types';

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

/** One request notice per request, in the window that filed it; clicking opens the Decisions. */
function announce(r: AgentRequest) {
  void notify('An agent asks for your approval', r.summary, 'info', { tabId: r.tab_id }, {
    key: `agent-request:${r.id}`,
    action: () => window.dispatchEvent(new CustomEvent('open-loom', { detail: { mode: 'decisions' } })),
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
async function createTab(a: { tabId?: string; workspace_id?: string; cwd?: string; runtime?: string; name?: string; reason?: string }): Promise<ToolResult> {
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
  if (!cwd.startsWith('/')) return { error: 'cwd must be an absolute path on this machine.' };
  if (!(await commands.isDirectory(cwd).catch(() => false))) return { error: `${cwd} is not a folder on this machine.` };
  const name = a.name?.trim().slice(0, MAX_TAB_NAME) || null;
  const agent = getDescriptor(runtime).displayName;
  // Only a NAMED agent tab is a mesh member (agentMesh `isAgentMember`).
  const joins = !!name && !!meshKey(ws);
  const effects = [`A new ${agent} session starts in ${cwd}, with its own context and the same access to this machine as any tab you open.`];
  if (joins) {
    effects.push(`It joins the mesh of ${names(meshOf(ws))}: ${count(meshOf(ws).flatMap(agentTabs).length, 'agent')} there can message it, and it them.`);
  }
  return file(
    a.tabId,
    'tab_create',
    { workspace_id: ws.id, cwd, runtime, name, mesh: joins ? snapshot(meshOf(ws)) : null },
    `Open a new ${agent} tab${name ? ` “${name}”` : ''} in “${ws.name}”`,
    effects,
    a.reason,
  );
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
      if (!(await commands.isDirectory(s('cwd')).catch(() => false))) throw new Error(`${s('cwd')} is no longer a folder`);
      const id = await workspacesStore.openAgentTab(ws.id, s('cwd'), s('runtime') as AgentRuntime, name);
      if (!id) throw new Error('the workspace is gone');
      return `opened tab ${id}${name ? ` (“${name}”)` : ''} in “${ws.name}”; its agent is starting`;
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
  }
}

async function onApproved(r: AgentRequest) {
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
    /** Waiting for the human, oldest first. */
    get pending(): AgentRequest[] {
      return requests.filter((r) => r.status === 'pending');
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
    restartMaiterm,
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
