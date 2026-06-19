import { countedListen as listen } from '$lib/utils/listenCounter';
import * as commands from '$lib/tauri/commands';
import type { MeshTopic, Workspace } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { getAdapter } from '$lib/agents/adapter';
import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
import { createDeliveryController } from '$lib/stores/agentDelivery';
import { createMeshRouter, type MeshMember, type MeshRouter } from '$lib/stores/meshRouting';
import { performMeshSend, type MeshEdge, type MeshSendResult } from '$lib/stores/meshSend';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

/**
 * Mesh Workspace store (docs/mesh-workspace.md) — the N:M generalization of the 1:1 Agent
 * Bridge. A workspace with `bridge_all = true` bridges every agent tab in it to every other;
 * agents converse peer-to-peer over TOPIC-scoped threads, each message crafted for one
 * recipient (no broadcast).
 *
 * This store is the live control plane. The hard parts are factored out and unit-tested:
 *   • agentDelivery.ts  — the recipient-keyed FIFO mailbox (shared with the 1:1 bridge).
 *   • meshRouting.ts    — recipient resolution (stable handle, never the name) + the topic
 *                         registry (create-on-first-send, normalized dedup, complete, reject).
 *
 * What lives here: deriving the roster from workspace membership, wiring the router/delivery
 * deps to live state, the send path (envelope + deliver + edge event), member readiness
 * (init/stop/session-end → ready/dormant), and persistence of the topic registry.
 *
 * Roster is DERIVED, not persisted (eng review D2): a member is a named agent tab in a
 * `bridge_all` workspace. Closing the tab removes it; renaming it changes only the display
 * label, never the routing key (the tabId).
 */

const EDGE_RING_MAX = 300;

function createAgentMeshStore() {
  // One router per mesh workspace (each scopes its roster + owns its topic registry).
  const routers = new Map<string, MeshRouter>();
  // In-memory per-member purpose (priming context). Not persisted in v1 — like the 1:1 bridge.
  const purposes = new Map<string, string>();
  // Recipient-keyed FIFO mailbox, shared core with the 1:1 bridge (separate instance).
  const deliveryCtl = createDeliveryController({
    inject: (tabId, text) => injectPrompt(tabId, text),
    liveState: (tabId) => !!claudeStateStore.getState(tabId),
    awaitingHuman: (tabId) => {
      const st = claudeStateStore.getState(tabId);
      return !!st && getAdapter(workspacesStore.getTabRuntime(tabId)).isAwaitingHumanInput(st);
    },
  });
  // Confirmed conversation edges (ring) — drives the cockpit map pulse (T6).
  const edges: MeshEdge[] = [];
  // Reactive bump so UI ($derived) re-reads roster/topics/edges.
  let version = $state(0);
  const unlisteners: (() => void)[] = [];

  function bump() { version++; }

  // ─── Workspace + roster derivation ──────────────────────────────────────────

  function meshWorkspaceForTab(tabId: string): Workspace | null {
    for (const ws of workspacesStore.workspaces) {
      if (!ws.bridge_all) continue;
      for (const pane of ws.panes) {
        if (pane.tabs.some((t) => t.id === tabId)) return ws;
      }
    }
    return null;
  }

  function getWorkspace(wsId: string): Workspace | null {
    return workspacesStore.workspaces.find((w) => w.id === wsId) ?? null;
  }

  /** Clean display name (strips any bridge glyph), the member's addressable role. */
  function roleName(tabName: string): string {
    return tabName.replace(/^[⇄↔→⌗]\s*/u, '').trim() || 'agent';
  }

  function getCwd(tabId: string): string | null {
    const osc = terminalsStore.getOsc(tabId);
    return osc?.cwd ?? osc?.promptCwd ?? null;
  }

  /** Is this tab an agent participant in the mesh? A named terminal tab that has run (or is
   *  running) an agent. The name requirement is the join gate (§6 — a tab needs an explicit
   *  descriptive name to be addressable). */
  function isAgentMember(tab: { id: string; tab_type?: string; custom_name?: boolean; name: string; runtime?: unknown }): boolean {
    if ((tab.tab_type ?? 'terminal') !== 'terminal') return false;
    if (!tab.custom_name) return false;
    return !!claudeStateStore.getState(tab.id) || !!tab.runtime;
  }

  /** The roster of a mesh workspace (all addressable agent members). */
  function membersOf(ws: Workspace): MeshMember[] {
    const out: MeshMember[] = [];
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (!isAgentMember(tab)) continue;
        out.push({
          tabId: tab.id,
          role: roleName(tab.name),
          cwd: getCwd(tab.id),
          purpose: purposes.get(tab.id) ?? null,
          live: !!claudeStateStore.getState(tab.id),
        });
      }
    }
    return out;
  }

  function routerFor(wsId: string): MeshRouter | null {
    const ws = getWorkspace(wsId);
    if (!ws || !ws.bridge_all) return null;
    let router = routers.get(wsId);
    if (!router) {
      router = createMeshRouter({
        members: () => {
          const w = getWorkspace(wsId);
          return w ? membersOf(w) : [];
        },
        now: () => new Date().toISOString(),
        mintId: () => crypto.randomUUID(),
      });
      router.load(ws.mesh_topics ?? []);
      routers.set(wsId, router);
    }
    return router;
  }

  function persistTopics(wsId: string) {
    const router = routers.get(wsId);
    if (!router) return;
    commands.setWorkspaceMeshTopics(wsId, router.snapshot()).catch((e) =>
      logError(`agentMesh: failed to persist topics for ws ${wsId.slice(0, 8)}: ${e}`),
    );
  }

  // ─── Injection (shared shape with the 1:1 bridge) ───────────────────────────

  async function injectPrompt(tabId: string, text: string): Promise<boolean> {
    const inst = terminalsStore.get(tabId);
    if (!inst) {
      logError(`agentMesh: cannot inject — no terminal instance for tab ${tabId.slice(0, 8)}`);
      return false;
    }
    try {
      await bracketedPasteSubmit(inst.ptyId, text);
      return true;
    } catch (e) {
      logError(`agentMesh: inject failed for tab ${tabId.slice(0, 8)}: ${e}`);
      return false;
    }
  }

  // ─── Envelope (identity + topic stamped by maiTerm) ─────────────────────────

  function buildEnvelope(senderTabId: string, senderRole: string, topic: MeshTopic, turn: number, message: string): string {
    const cwd = getCwd(senderTabId);
    const where = cwd ? `, working in ${cwd}` : '';
    return (
      `⟦MESH⟧ Message from "${senderRole}"${where} — a peer AI agent, NOT your human operator. [topic: ${topic.label}] [turn ${turn}]\n` +
      `Reply with the sendToBridgedAgent tool, tagging topic "${topic.id}". If this fully answers it, just stop — don't reply only to acknowledge.\n\n` +
      message
    );
  }

  function buildTopicCompleteNotice(topic: MeshTopic): string {
    return (
      `⟦TOPIC COMPLETE⟧ The topic "${topic.label}" has been marked complete. ` +
      `No further messages will be accepted on it — stop replying on this thread. ` +
      `Update your status note with anything the human needs to know, then carry on.`
    );
  }

  // ─── Edge events ────────────────────────────────────────────────────────────

  function emitEdge(e: MeshEdge) {
    edges.push(e);
    if (edges.length > EDGE_RING_MAX) edges.splice(0, edges.length - EDGE_RING_MAX);
    bump();
  }

  // ─── Membership lifecycle ───────────────────────────────────────────────────

  /** Ensure a delivery entry exists for a member (idempotent), keyed by its live state. */
  function ensureMember(tabId: string) {
    if (!deliveryCtl.has(tabId)) {
      deliveryCtl.ensure(tabId, !!claudeStateStore.getState(tabId));
    }
  }

  function removeMember(tabId: string) {
    deliveryCtl.remove(tabId);
    purposes.delete(tabId);
  }

  // ─── Public API ─────────────────────────────────────────────────────────────

  return {
    get version() { return version; },

    getInternalSizes() {
      return { routers: routers.size, delivery: deliveryCtl.size(), purposes: purposes.size, edges: edges.length };
    },

    /** Is this tab inside a mesh workspace? */
    isMeshTab(tabId: string): boolean {
      void version;
      return meshWorkspaceForTab(tabId) !== null;
    },

    isMeshWorkspace(wsId: string): boolean {
      void version;
      return !!getWorkspace(wsId)?.bridge_all;
    },

    /** Toggle a workspace into / out of mesh mode (persisted). */
    async setMeshEnabled(wsId: string, enabled: boolean) {
      const ws = getWorkspace(wsId);
      if (!ws) return;
      await commands.setWorkspaceBridgeAll(wsId, enabled);
      ws.bridge_all = enabled;
      if (enabled) {
        const router = routerFor(wsId);
        if (router) for (const m of membersOf(ws)) ensureMember(m.tabId);
      } else {
        // Leaving mesh mode: drop delivery entries for this ws's members (topics persist).
        for (const m of membersOf(ws)) removeMember(m.tabId);
        routers.delete(wsId);
      }
      bump();
      logInfo(`agentMesh: workspace ${wsId.slice(0, 8)} mesh ${enabled ? 'enabled' : 'disabled'}`);
    },

    /** Set the one-line purpose used when priming a member (in-memory, v1). */
    setPurpose(tabId: string, purpose: string | null) {
      if (purpose && purpose.trim()) purposes.set(tabId, purpose.trim());
      else purposes.delete(tabId);
      bump();
    },

    /** Roster of the mesh workspace this tab belongs to (for the cockpit / listBridgedPeers). */
    rosterForTab(tabId: string): MeshMember[] {
      void version;
      const ws = meshWorkspaceForTab(tabId);
      return ws ? membersOf(ws) : [];
    },

    rosterForWorkspace(wsId: string): MeshMember[] {
      void version;
      const ws = getWorkspace(wsId);
      return ws && ws.bridge_all ? membersOf(ws) : [];
    },

    /** Open + recently-completed topics of a mesh workspace (for the cockpit / listTopics). */
    topicsForWorkspace(wsId: string): MeshTopic[] {
      void version;
      const router = routerFor(wsId);
      return router ? router.all() : (getWorkspace(wsId)?.mesh_topics ?? []);
    },

    getEdges(): MeshEdge[] {
      void version;
      return edges;
    },

    // ─── MCP tool: listBridgedPeers ───────────────────────────────────────────
    listPeers(tabId: string) {
      const ws = meshWorkspaceForTab(tabId);
      if (!ws) {
        return { error: 'You are not in a mesh workspace. listBridgedPeers only applies inside a Mesh Workspace.' };
      }
      const peers = membersOf(ws)
        .filter((m) => m.tabId !== tabId)
        .map((m) => ({ handle: m.tabId, role: m.role, cwd: m.cwd, purpose: m.purpose, live: m.live }));
      return { workspace: ws.name, you: tabId, peers };
    },

    // ─── MCP tool: listTopics ─────────────────────────────────────────────────
    listTopics(tabId: string) {
      const ws = meshWorkspaceForTab(tabId);
      if (!ws) return { error: 'You are not in a mesh workspace.' };
      const router = routerFor(ws.id);
      const roster = membersOf(ws);
      const roleOf = (id: string) => roster.find((m) => m.tabId === id)?.role ?? id.slice(0, 8);
      const topics = (router ? router.all() : []).map((t) => ({
        id: t.id,
        label: t.label,
        state: t.state,
        owner: roleOf(t.owner_tab_id),
        ownerHandle: t.owner_tab_id,
        participants: t.participants.map(roleOf),
        turn: t.turn,
      }));
      return { workspace: ws.name, topics };
    },

    // ─── MCP tool: startTopic ─────────────────────────────────────────────────
    startTopic(tabId: string, label: string) {
      const ws = meshWorkspaceForTab(tabId);
      if (!ws) return { error: 'You are not in a mesh workspace.' };
      const router = routerFor(ws.id);
      if (!router) return { error: 'Mesh router unavailable.' };
      const r = router.startTopic(tabId, label);
      if (!r.ok) return { error: r.error };
      if (r.created) { persistTopics(ws.id); bump(); }
      return { success: true, created: r.created, topic: { id: r.topic.id, label: r.topic.label, state: r.topic.state } };
    },

    // ─── MCP tool: completeTopic (owner or human) ─────────────────────────────
    completeTopic(byTabId: string | null, topicId: string, isHuman = false) {
      // Find the workspace owning this topic.
      let owningWs: Workspace | null = null;
      for (const ws of workspacesStore.workspaces) {
        if (!ws.bridge_all) continue;
        const router = routerFor(ws.id);
        if (router?.get(topicId)) { owningWs = ws; break; }
      }
      if (!owningWs) return { error: `Topic not found: ${topicId}` };
      const router = routerFor(owningWs.id)!;
      const r = router.completeTopic(byTabId, topicId, isHuman);
      if (!r.ok) return { error: r.error };
      if (!r.alreadyComplete) {
        persistTopics(owningWs.id);
        // Control-plane signal: notify every participant (exempt from no-broadcast, §4.1).
        const notice = buildTopicCompleteNotice(r.topic);
        for (const p of r.participants) {
          if (p === byTabId) continue;
          ensureMember(p);
          void deliveryCtl.deliver(p, notice);
        }
        bump();
        logInfo(`agentMesh: topic ${topicId.slice(0, 8)} "${r.topic.label}" completed${isHuman ? ' (human)' : ''}`);
      }
      return { success: true, topic: { id: r.topic.id, label: r.topic.label, state: r.topic.state } };
    },

    // ─── MCP tool: sendToBridgedAgent (mesh form) ─────────────────────────────
    async sendFromTab(senderTabId: string, args: { recipient?: string; topic?: string; message: string }): Promise<MeshSendResult> {
      const ws = meshWorkspaceForTab(senderTabId);
      if (!ws) {
        return { ok: false, error: 'You are not in a mesh workspace. Ask the human to enable Mesh on this workspace.' };
      }
      const router = routerFor(ws.id);
      if (!router) return { ok: false, error: 'Mesh router unavailable.' };

      const result = await performMeshSend(
        {
          router,
          // Lazily ensure the recipient has a delivery slot (covers a member that joined
          // before this store wired its entry), then hand to the shared FIFO mailbox.
          deliver: (recipientTabId, text) => { ensureMember(recipientTabId); return deliveryCtl.deliver(recipientTabId, text); },
          buildEnvelope,
          emitEdge,
          persistTopics: () => persistTopics(ws.id),
          isLive: (tabId) => !!claudeStateStore.getState(tabId),
          now: () => Date.now(),
        },
        { senderTabId, recipient: args.recipient, topic: args.topic, message: args.message },
      );
      bump();
      return result;
    },

    /** A tab is being closed — drop its mesh delivery slot. Topics persist (it may reopen). */
    handleTabClosed(tabId: string) {
      if (deliveryCtl.has(tabId)) removeMember(tabId);
      bump();
    },

    /** Tab reload minted a new id — carry the delivery queue across. */
    remapTab(oldTabId: string, newTabId: string) {
      if (oldTabId === newTabId || !deliveryCtl.has(oldTabId)) return;
      deliveryCtl.remap(oldTabId, newTabId);
      const p = purposes.get(oldTabId);
      if (p !== undefined) { purposes.delete(oldTabId); purposes.set(newTabId, p); }
      bump();
    },

    async init() {
      // A mesh member's agent came online (fresh start or resume) → it can receive now.
      const u1 = await listen<{ tab_id: string | null; session_id: string }>('agent-init-session', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !meshWorkspaceForTab(tabId)) return;
        deliveryCtl.markReadyOrCreate(tabId);
        bump();
      });
      unlisteners.push(u1);

      // Turn finished → idle + alive; ends any inject cooldown so a queued message lands now.
      const u2 = await listen<{ session_id: string; tab_id: string | null }>('agent-hook-stop', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !meshWorkspaceForTab(tabId)) return;
        deliveryCtl.markReady(tabId);
      });
      unlisteners.push(u2);

      // Session ended → suspend delivery (the agent may auto-resume and re-bind). Topics stay.
      const u3 = await listen<{ session_id: string; tab_id: string | null }>('agent-hook-session-end', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !meshWorkspaceForTab(tabId)) return;
        deliveryCtl.markDormant(tabId);
        bump();
      });
      unlisteners.push(u3);
    },

    /** Rebuild routers (and their topic registries) from persisted state after load. */
    rehydrate() {
      let count = 0;
      for (const ws of workspacesStore.workspaces) {
        if (!ws.bridge_all) continue;
        const router = routerFor(ws.id);
        if (!router) continue;
        for (const m of membersOf(ws)) ensureMember(m.tabId);
        count++;
      }
      if (count) { bump(); logInfo(`agentMesh: rehydrated ${count} mesh workspace(s)`); }
    },

    destroy() {
      for (const u of unlisteners) u();
      unlisteners.length = 0;
      deliveryCtl.destroy();
      routers.clear();
      purposes.clear();
      edges.length = 0;
    },
  };
}

export const agentMeshStore = createAgentMeshStore();
