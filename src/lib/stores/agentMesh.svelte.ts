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
import { createLoopController, type LoopReason } from '$lib/stores/meshLoopControl';
import { statusMarker, buildStatusNoteTemplate, parseNeedsDecision, parseStatusNote } from '$lib/stores/meshStatus';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { dispatch as dispatchNotification } from '$lib/stores/notificationDispatch';
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
  // Members already primed this session (opener injected) — keyed by tabId, idempotent.
  const primed = new Set<string>();
  // Each member's status-note id (the one workspace note it maintains), keyed by tabId.
  const statusNoteIds = new Map<string, string>();
  // Last "NEEDS DECISION" text surfaced per status note, to dedupe the decision toast.
  const lastDecision = new Map<string, string>();
  // Stage-view UI state per mesh workspace (T7): which two members are on the stage, and
  // whether the stage/filmstrip layout is active (vs normal splits). In-memory UI state.
  interface StageState { active: boolean; left: string | null; right: string | null; }
  const stage = new Map<string, StageState>();
  // Recipient-keyed FIFO mailbox, shared core with the 1:1 bridge (separate instance).
  const deliveryCtl = createDeliveryController({
    inject: (tabId, text) => injectPrompt(tabId, text),
    liveState: (tabId) => !!claudeStateStore.getState(tabId),
    awaitingHuman: (tabId) => {
      const st = claudeStateStore.getState(tabId);
      return !!st && getAdapter(workspacesStore.getTabRuntime(tabId)).isAwaitingHumanInput(st);
    },
  });
  // Per-topic loop control (§10): soft cap + hard ceiling + TTL, limits live from prefs.
  const loopCtl = createLoopController({
    limits: () => ({
      softCap: preferencesStore.meshSoftCap,
      hardCap: preferencesStore.meshHardCap,
      ttlMs: preferencesStore.meshTopicTtlMinutes * 60_000,
    }),
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
          purpose: tab.mesh_purpose ?? null,
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

  // ─── Priming + status notes (§6, §8) ────────────────────────────────────────

  function buildMeshOpener(member: MeshMember, peers: MeshMember[], noteId: string): string {
    const where = member.cwd ? ` (working in ${member.cwd})` : '';
    const purpose = member.purpose?.trim();
    const roster = peers.length
      ? peers.map((p) => `  - "${p.role}"${p.purpose ? ` — ${p.purpose}` : p.cwd ? ` — ${p.cwd}` : ''}`).join('\n')
      : '  (no other agents yet — peers appear as they join; call listBridgedPeers anytime)';
    return (
      `⟦MESH⟧ You've joined a Mesh Workspace as "${member.role}"${where}. Every agent here is a peer AI agent (NOT your human operator); you can talk to any of them.\n\n` +
      `Your purpose: ${purpose || '(your human will tell you — ask if unclear)'}\n\n` +
      `Peers you can reach:\n${roster}\n\n` +
      `How the mesh works:\n` +
      `  - Every message goes to ONE peer (no broadcast) and is tagged with a TOPIC. Start a thread by passing a short topic label to sendToBridgedAgent (you own it), or reply on an existing topic id from listTopics. Always tag a reply with the topic id shown in the incoming message.\n` +
      `  - Reusing a thread keeps context together; near-duplicate labels are deduped automatically.\n` +
      `  - When a thread's work is done, its OWNER calls completeTopic(id) so peers stop replying. Don't reply just to acknowledge.\n` +
      `  - Tools: listBridgedPeers, listTopics, startTopic, completeTopic, sendToBridgedAgent (recipient = a peer's role or handle; topic = id or new label).\n\n` +
      `Your status note (id: ${noteId}): keep this ONE workspace note updated as you work — what you've completed, what's blocked, and anything needing the human under a line starting "NEEDS DECISION:". Use writeWorkspaceNote with noteId "${noteId}". Be concise; leave the first marker line intact.\n\n` +
      `Don't message anyone yet. First check in with your human: confirm you've joined as "${member.role}", say what you'll own, and wait for direction.`
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
  }

  /** Ensure this member has its one status note, reusing an existing one (by role marker)
   *  rather than spawning duplicates across re-prime / restart. Returns whether it was just
   *  created — a freshly created note means a genuinely new join (→ prime), an existing one
   *  means the agent was onboarded before (→ don't re-inject the opener). */
  async function ensureStatusNote(ws: Workspace, member: MeshMember): Promise<{ id: string; created: boolean } | null> {
    const known = statusNoteIds.get(member.tabId);
    if (known && ws.workspace_notes.some((n) => n.id === known)) return { id: known, created: false };
    const marker = statusMarker(member.role);
    const byMarker = ws.workspace_notes.find((n) => n.content.startsWith(marker));
    if (byMarker) { statusNoteIds.set(member.tabId, byMarker.id); return { id: byMarker.id, created: false }; }
    const note = await workspacesStore.addWorkspaceNote(ws.id, buildStatusNoteTemplate(member.role, member.purpose), 'preview');
    if (!note) return null;
    statusNoteIds.set(member.tabId, note.id);
    return { id: note.id, created: true };
  }

  /** Prime a member on join: pre-create its status note and inject the mesh opener once.
   *  Idempotent (guarded by `primed`); skips the opener for an agent that was already
   *  onboarded in a prior session (its status note already exists). */
  async function tryPrime(tabId: string) {
    if (primed.has(tabId)) return;
    const ws = meshWorkspaceForTab(tabId);
    if (!ws) return;
    const member = membersOf(ws).find((m) => m.tabId === tabId);
    if (!member || !member.live) return; // not a named, live agent yet — re-check on next Stop
    primed.add(tabId); // mark before the await so a racing event can't double-prime
    const note = await ensureStatusNote(ws, member);
    if (!note) { primed.delete(tabId); return; } // note creation failed — allow a retry
    ensureMember(tabId);
    if (note.created) {
      const peers = membersOf(ws).filter((m) => m.tabId !== tabId);
      const status = await deliveryCtl.deliver(tabId, buildMeshOpener(member, peers, note.id));
      if (status === 'failed') { primed.delete(tabId); return; }
      logInfo(`agentMesh: primed "${member.role}" (${tabId.slice(0, 8)}) into mesh "${ws.name}"`);
    }
    bump();
  }

  /** Scan a just-written status note for a NEEDS DECISION block and raise a toast (deduped).
   *  Called from the workspace-note write path (§8 — pull the human in instead of watching). */
  function scanDecision(ws: Workspace, noteId: string, content: string) {
    // Which member owns this status note (for the deep-link + role label)?
    let ownerTabId: string | null = null;
    let ownerRole = 'An agent';
    for (const [tabId, nid] of statusNoteIds) {
      if (nid !== noteId) continue;
      ownerTabId = tabId;
      ownerRole = membersOf(ws).find((m) => m.tabId === tabId)?.role ?? ownerRole;
      break;
    }
    const decision = parseNeedsDecision(content);
    if (!decision) { lastDecision.delete(noteId); return; }
    if (lastDecision.get(noteId) === decision) return; // already surfaced this exact text
    lastDecision.set(noteId, decision);
    void dispatchNotification(`${ownerRole} needs a decision`, decision, 'info', ownerTabId ? { tabId: ownerTabId } : undefined);
  }

  // ─── Loop-control pause inspection (for the cockpit) ────────────────────────

  /** Is this topic currently paused (would its NEXT turn be gated)? Open topics only. */
  function pauseInfo(topic: MeshTopic): { paused: boolean; reason?: LoopReason; turn: number; cap: number } {
    if (topic.state !== 'open') return { paused: false, turn: topic.turn, cap: 0 };
    const v = loopCtl.evaluate(topic.id, topic.turn + 1, Date.parse(topic.created_at) || Date.now(), Date.now());
    return v.ok ? { paused: false, turn: topic.turn, cap: 0 } : { paused: true, reason: v.reason, turn: v.turn, cap: v.cap };
  }

  /** Find a topic (and its workspace) by id across all mesh workspaces. */
  function findTopicById(topicId: string): { ws: Workspace; topic: MeshTopic } | null {
    for (const ws of workspacesStore.workspaces) {
      if (!ws.bridge_all) continue;
      const router = routerFor(ws.id);
      const topic = router?.get(topicId);
      if (topic) return { ws, topic };
    }
    return null;
  }

  // ─── Public API ─────────────────────────────────────────────────────────────

  return {
    get version() { return version; },

    getInternalSizes() {
      return { routers: routers.size, delivery: deliveryCtl.size(), edges: edges.length };
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
        if (router) for (const m of membersOf(ws)) { ensureMember(m.tabId); void tryPrime(m.tabId); }
      } else {
        // Leaving mesh mode: drop delivery entries for this ws's members (topics persist).
        for (const m of membersOf(ws)) { removeMember(m.tabId); primed.delete(m.tabId); statusNoteIds.delete(m.tabId); }
        routers.delete(wsId);
      }
      bump();
      logInfo(`agentMesh: workspace ${wsId.slice(0, 8)} mesh ${enabled ? 'enabled' : 'disabled'}`);
    },

    /** Set a member's one-line purpose (persisted on the tab so it survives restart). */
    setPurpose(tabId: string, purpose: string | null) {
      const clean = purpose && purpose.trim() ? purpose.trim() : null;
      // Locate the tab and mutate it for immediate reactivity, then persist.
      for (const ws of workspacesStore.workspaces) {
        for (const pane of ws.panes) {
          const tab = pane.tabs.find((t) => t.id === tabId);
          if (tab) {
            tab.mesh_purpose = clean;
            commands.setTabMeshPurpose(ws.id, pane.id, tabId, clean).catch((e) =>
              logError(`agentMesh: failed to persist purpose for tab ${tabId.slice(0, 8)}: ${e}`),
            );
            bump();
            return;
          }
        }
      }
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

    /** The status board for the cockpit: each member with its parsed status note + claude
     *  state. Pairs the derived roster with each agent's one workspace note (§8). */
    statusBoard(wsId: string) {
      void version;
      const ws = getWorkspace(wsId);
      if (!ws || !ws.bridge_all) return [];
      return membersOf(ws).map((m) => {
        const noteId = statusNoteIds.get(m.tabId)
          ?? ws.workspace_notes.find((n) => n.content.startsWith(statusMarker(m.role)))?.id
          ?? null;
        const note = noteId ? ws.workspace_notes.find((n) => n.id === noteId) : undefined;
        const parsed = note ? parseStatusNote(note.content) : { done: [], needsDecision: [], blocked: [] };
        const cs = claudeStateStore.getState(m.tabId);
        return {
          tabId: m.tabId,
          role: m.role,
          cwd: m.cwd,
          purpose: m.purpose,
          live: m.live,
          claudeState: cs?.state ?? null,
          noteId,
          ...parsed,
        };
      });
    },

    /** Workspaces in this window that are meshes (for the cockpit's workspace resolution). */
    meshWorkspaces(): { id: string; name: string }[] {
      void version;
      return workspacesStore.workspaces.filter((w) => w.bridge_all).map((w) => ({ id: w.id, name: w.name }));
    },

    // ─── Stage view (T7): two-panel stage + scaled filmstrip ──────────────────

    /** Is the stage/filmstrip layout active for this workspace? */
    isStageView(wsId: string): boolean {
      void version;
      return !!stage.get(wsId)?.active;
    },

    /** Current stage occupants (left/right tabIds), validated against live membership. */
    stageSlots(wsId: string): { left: string | null; right: string | null } {
      void version;
      const s = stage.get(wsId);
      if (!s) return { left: null, right: null };
      const ws = getWorkspace(wsId);
      const memberIds = new Set(ws ? membersOf(ws).map((m) => m.tabId) : []);
      return { left: s.left && memberIds.has(s.left) ? s.left : null, right: s.right && memberIds.has(s.right) ? s.right : null };
    },

    /** Turn the stage layout on/off for a mesh workspace; seeds the two slots on first on. */
    toggleStageView(wsId: string) {
      const ws = getWorkspace(wsId);
      if (!ws || !ws.bridge_all) return;
      const s = stage.get(wsId) ?? { active: false, left: null, right: null };
      s.active = !s.active;
      if (s.active) {
        const members = membersOf(ws).map((m) => m.tabId);
        if (!s.left || !members.includes(s.left)) s.left = members[0] ?? null;
        if (!s.right || !members.includes(s.right) || s.right === s.left) s.right = members.find((m) => m !== s.left) ?? null;
      }
      stage.set(wsId, s);
      bump();
    },

    /** Promote a member to a stage slot (click → left, shift+click → right). The previous
     *  occupant of that slot falls back to the filmstrip; promoting a tab already on the
     *  other slot swaps the two so a terminal is never on both. */
    promoteToStage(wsId: string, tabId: string, side: 'left' | 'right') {
      const s = stage.get(wsId);
      if (!s) return;
      const other = side === 'left' ? 'right' : 'left';
      if (s[other] === tabId) s[other] = s[side]; // swap rather than duplicate
      s[side] = tabId;
      stage.set(wsId, s);
      bump();
    },

    /** Is this tab currently on a stage slot of an ACTIVE stage view? */
    isOnStage(tabId: string): boolean {
      void version;
      const ws = meshWorkspaceForTab(tabId);
      if (!ws) return false;
      const s = stage.get(ws.id);
      return !!s?.active && (s.left === tabId || s.right === tabId);
    },

    /** Is this tab an addressable member of its mesh workspace? Drives `visible` in +page so
     *  ALL members render live in stage view (stage at scale 1, filmstrip CSS-scaled). */
    isMeshMemberTab(tabId: string): boolean {
      void version;
      const ws = meshWorkspaceForTab(tabId);
      return !!ws && membersOf(ws).some((m) => m.tabId === tabId);
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
      const topics = (router ? router.all() : []).map((t) => {
        const pause = pauseInfo(t);
        return {
          id: t.id,
          label: t.label,
          state: t.state,
          owner: roleOf(t.owner_tab_id),
          ownerHandle: t.owner_tab_id,
          participants: t.participants.map(roleOf),
          turn: t.turn,
          ...(pause.paused ? { paused: true, pauseReason: pause.reason } : {}),
        };
      });
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
        loopCtl.clear(topicId);
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

    // ─── Cockpit: loop-control resume + pause inspection (human-driven) ────────

    /** Human lifts a paused topic's soft cap (and re-bases its TTL) so it flows again. */
    resumeTopic(topicId: string): { success: true; topic: { id: string; label: string } } | { error: string } {
      const ctx = findTopicById(topicId);
      if (!ctx) return { error: `Topic not found: ${topicId}` };
      if (ctx.topic.state === 'complete') return { error: 'Topic is already complete.' };
      loopCtl.resume(topicId, Date.now());
      bump();
      logInfo(`agentMesh: topic ${topicId.slice(0, 8)} "${ctx.topic.label}" resumed by human`);
      return { success: true, topic: { id: ctx.topic.id, label: ctx.topic.label } };
    },

    /** Pause state of a topic (for the cockpit resume button). */
    topicPauseInfo(topicId: string): { paused: boolean; reason?: LoopReason; turn: number; cap: number } {
      void version;
      const ctx = findTopicById(topicId);
      return ctx ? pauseInfo(ctx.topic) : { paused: false, turn: 0, cap: 0 };
    },

    /** All currently-paused open topics in a workspace (for the cockpit banner). */
    pausedTopics(wsId: string): { id: string; label: string; reason: LoopReason; turn: number; cap: number }[] {
      void version;
      const router = routerFor(wsId);
      if (!router) return [];
      const out: { id: string; label: string; reason: LoopReason; turn: number; cap: number }[] = [];
      for (const t of router.all()) {
        const p = pauseInfo(t);
        if (p.paused && p.reason) out.push({ id: t.id, label: t.label, reason: p.reason, turn: p.turn, cap: p.cap });
      }
      return out;
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
          gate: (topic, nextTurn) =>
            loopCtl.evaluate(topic.id, nextTurn, Date.parse(topic.created_at) || Date.now(), Date.now()),
        },
        { senderTabId, recipient: args.recipient, topic: args.topic, message: args.message },
      );
      bump();
      return result;
    },

    /** A tab is being closed — drop its mesh delivery slot. Topics persist (it may reopen). */
    handleTabClosed(tabId: string) {
      if (deliveryCtl.has(tabId)) removeMember(tabId);
      primed.delete(tabId);
      const nid = statusNoteIds.get(tabId);
      statusNoteIds.delete(tabId);
      if (nid) lastDecision.delete(nid);
      for (const s of stage.values()) { if (s.left === tabId) s.left = null; if (s.right === tabId) s.right = null; }
      bump();
    },

    /** Tab reload minted a new id — carry the delivery queue + priming state across. */
    remapTab(oldTabId: string, newTabId: string) {
      if (oldTabId === newTabId || !deliveryCtl.has(oldTabId)) return;
      deliveryCtl.remap(oldTabId, newTabId);
      if (primed.has(oldTabId)) { primed.delete(oldTabId); primed.add(newTabId); }
      const nid = statusNoteIds.get(oldTabId);
      if (nid !== undefined) { statusNoteIds.delete(oldTabId); statusNoteIds.set(newTabId, nid); }
      for (const s of stage.values()) { if (s.left === oldTabId) s.left = newTabId; if (s.right === oldTabId) s.right = newTabId; }
      bump();
    },

    /** Called from the workspace-note write path: scan a mesh status note for a NEEDS
     *  DECISION block and surface a toast (§8). No-op outside a mesh workspace. */
    onWorkspaceNoteWritten(wsId: string, noteId: string, content: string) {
      const ws = getWorkspace(wsId);
      if (!ws || !ws.bridge_all) return;
      scanDecision(ws, noteId, content);
    },

    async init() {
      // A mesh member's agent came online (fresh start or resume) → it can receive now.
      const u1 = await listen<{ tab_id: string | null; session_id: string }>('agent-init-session', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !meshWorkspaceForTab(tabId)) return;
        deliveryCtl.markReadyOrCreate(tabId);
        void tryPrime(tabId); // a member just came online → prime it (idempotent)
        bump();
      });
      unlisteners.push(u1);

      // Turn finished → idle + alive; ends any inject cooldown so a queued message lands now.
      // Also a re-check point: an agent named AFTER it started becomes primeable here.
      const u2 = await listen<{ session_id: string; tab_id: string | null }>('agent-hook-stop', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !meshWorkspaceForTab(tabId)) return;
        deliveryCtl.markReady(tabId);
        void tryPrime(tabId);
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
      loopCtl.reset();
      routers.clear();
      primed.clear();
      statusNoteIds.clear();
      lastDecision.clear();
      stage.clear();
      edges.length = 0;
    },
  };
}

export const agentMeshStore = createAgentMeshStore();
