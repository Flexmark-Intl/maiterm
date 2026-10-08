import { countedListen as listen } from '$lib/utils/listenCounter';
import * as commands from '$lib/tauri/commands';
import type { MeshTopic, Workspace } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { getAdapter } from '$lib/agents/adapter';
import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
import { waitQuiet } from '$lib/agents/wake';
import { agentDelivery as deliveryCtl, DELIVERY_OWNER_MESH as OWNER } from '$lib/stores/agentDeliveryLive';
import { createMeshRouter, roleName, MESH_ONBOARDED_VAR, MESH_FORMER_ROLES_VAR, type MeshMember, type MeshRouter } from '$lib/stores/meshRouting';
import { performMeshSend, type MeshEdge, type MeshSendResult } from '$lib/stores/meshSend';
import { createLoopController, type LoopReason } from '$lib/stores/meshLoopControl';
import { getVariables, setVariable, replayAutoResume } from '$lib/stores/triggers.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
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
 *
 * LINKED meshes (§17): mesh workspaces in this window sharing `Workspace.mesh_group` are ONE
 * mesh — the roster, the router and the topic registry span all of them (`meshWorkspacesOf`).
 * The stage view stays per workspace. A router is cached with the workspaces it was built
 * from and rebuilt when that set changes; each topic is persisted on its owner's workspace,
 * so an unlink splits the registry with nothing to migrate.
 */

const EDGE_RING_MAX = 300;
// Topic lifecycle hygiene: agents rarely call completeTopic, so without a sweep the cockpit
// list only ever grows. An open topic idle this long is auto-completed (silently — no
// ⟦TOPIC COMPLETE⟧ notice; its participants moved on long ago), and a completed topic is
// hard-deleted after a short retention (long enough to see the closure dimmed in the panel).
// Swept on rehydrate (app start) and hourly for long-running sessions.
const TOPIC_STALE_OPEN_MS = 7 * 24 * 60 * 60 * 1000;
const TOPIC_COMPLETED_RETENTION_MS = 48 * 60 * 60 * 1000;
const TOPIC_SWEEP_INTERVAL_MS = 60 * 60 * 1000;
// MESH_ONBOARDED_VAR (persisted per-tab trigger variable, see meshRouting.ts): an agent has
// been introduced to the mesh, so a resumed agent — whose transcript already holds the opener —
// isn't re-onboarded on every app restart. MESH_FORMER_ROLES_VAR: JSON list of the roles it was
// introduced under before a rename (MeshMember.formerRoles), newest last, capped so a
// much-renamed tab doesn't accrete forever.
const MESH_FORMER_ROLES_MAX = 5;

function createAgentMeshStore() {
  // One router per MESH (each scopes its roster + owns its topic registry), keyed by
  // `meshKeyOf` and remembering which workspaces it was built from — a link, an unlink, or a
  // workspace deleted/moved away changes that set, and the next `routerFor` rebuilds.
  interface RouterEntry { router: MeshRouter; wsIds: string[] }
  const routers = new Map<string, RouterEntry>();
  // Which workspace persists each topic (its owner's, at creation). Seeded on every rebuild
  // from where the topic was loaded; a topic without one is homed on first persist.
  const topicHome = new Map<string, string>();
  // A rebuild's seed: the newest in-memory copy of every topic any router held. A send's turn
  // bump isn't persisted on its own, so seeding from the mirror alone would roll turn counts
  // (and the loop cap riding on them) back on every link/unlink.
  const lastKnown = new Map<string, MeshTopic>();
  // Members already primed this session (opener injected) — keyed by tabId, idempotent.
  const primed = new Set<string>();
  // Stage-view UI state per mesh workspace (T7): which two members are on the stage, and
  // whether the stage/filmstrip layout is active (vs normal splits). In-memory UI state.
  interface StageState { active: boolean; left: string | null; right: string | null; }
  const stage = new Map<string, StageState>();
  // Mesh workspaces we've already offered an auto re-check for this session (so switching
  // between workspaces doesn't re-prompt). Cleared on destroy.
  const autoRechecked = new Set<string>();
  // Recipient-keyed FIFO mailbox: the ONE live controller (agentDeliveryLive.ts), shared
  // with the 1:1 bridge — a tab can be on a mesh AND hold a bridge, and one PTY needs one
  // inject guard. This store's slots are held under OWNER.
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

  function workspaceOfTab(tabId: string): Workspace | null {
    return workspacesStore.workspaces.find((w) => w.panes.some((p) => p.tabs.some((t) => t.id === tabId))) ?? null;
  }

  /** The workspaces whose agents make up this workspace's mesh, in sidebar order: every mesh
   *  workspace IN THIS WINDOW sharing its `mesh_group`, or just itself when it has none. A
   *  workspace that isn't a mesh counts as its own (for code that inspects one before it's
   *  enabled). Other windows are never part of it — a linked workspace moved to another
   *  window meshes there with whatever shares its group, and rejoins if it moves back. */
  function meshWorkspacesOf(ws: Workspace): Workspace[] {
    if (!ws.bridge_all || !ws.mesh_group) return [ws];
    const g = ws.mesh_group;
    return workspacesStore.workspaces.filter((w) => w.bridge_all && w.mesh_group === g);
  }

  function meshKeyOf(ws: Workspace): string {
    return ws.bridge_all && ws.mesh_group ? `group:${ws.mesh_group}` : ws.id;
  }

  /** Is this workspace's mesh linked with another workspace in this window? */
  function isLinked(ws: Workspace): boolean {
    return meshWorkspacesOf(ws).length > 1;
  }

  function formerRolesOf(tabId: string): string[] {
    const raw = getVariables(tabId)?.get(MESH_FORMER_ROLES_VAR);
    if (!raw) return [];
    try {
      const parsed: unknown = JSON.parse(raw);
      return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === 'string' && !!v) : [];
    } catch {
      return [];
    }
  }

  const sameRole = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

  /** Rewrite a tab's former-role list: never its current role, `add` (if given) moved to the
   *  end as the newest, capped. Writes only on change (a variable write re-evaluates
   *  variable-mode triggers, so don't churn it). */
  async function updateFormerRoles(tabId: string, currentRole: string, add: string | null) {
    const before = formerRolesOf(tabId);
    let next = before.filter((f) => !sameRole(f, currentRole) && (add === null || !sameRole(f, add)));
    if (add !== null && !sameRole(add, currentRole)) next.push(add);
    next = next.slice(-MESH_FORMER_ROLES_MAX);
    if (next.length === before.length && next.every((f, i) => f === before[i])) return;
    await setVariable(tabId, MESH_FORMER_ROLES_VAR, next.length ? JSON.stringify(next) : null);
  }

  /** Role names currently held by ANY custom-named terminal tab in the workspace — including
   *  one whose agent hasn't registered yet and so isn't a roster member. A former role that a
   *  tab has since claimed must not route to its previous holder (a silent misroute); until
   *  the new holder's agent is up the name is simply unknown, as it was before renames. */
  function claimedRoles(ws: Workspace): string[] {
    const out: string[] = [];
    for (const w of meshWorkspacesOf(ws)) {
      for (const pane of w.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') === 'terminal' && !tab.service_id && tab.custom_name) out.push(roleName(tab.name).toLowerCase());
        }
      }
    }
    return out;
  }

  function getCwd(tabId: string): string | null {
    const osc = terminalsStore.getOsc(tabId);
    return osc?.cwd ?? osc?.promptCwd ?? null;
  }

  /** The sender's addressable role, resolved from its tab. Identity is maiTerm-stamped from
   *  the tab itself (never a value the caller threads through) so the envelope's "from" can
   *  never be someone else's name. Falls back to a short handle if the tab has vanished. */
  function roleForTab(tabId: string): string {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find((t) => t.id === tabId);
        if (tab) return roleName(tab.name);
      }
    }
    return tabId.slice(0, 8);
  }

  /** Is this tab an agent participant in the mesh? A named terminal tab that has run (or is
   *  running) an agent. The name requirement is the join gate (§6 — a tab needs an explicit
   *  descriptive name to be addressable). */
  function isAgentMember(tab: { id: string; tab_type?: string; custom_name?: boolean; name: string; runtime?: unknown; service_id?: string | null }): boolean {
    if ((tab.tab_type ?? 'terminal') !== 'terminal' || tab.service_id) return false;
    if (!tab.custom_name) return false;
    return !!claudeStateStore.getState(tab.id) || !!tab.runtime;
  }

  /** The roster of the MESH this workspace belongs to — every addressable agent member of
   *  every workspace linked with it (§17). On a linked mesh each member names its workspace. */
  function membersOf(ws: Workspace): MeshMember[] {
    const out: MeshMember[] = [];
    const mesh = meshWorkspacesOf(ws);
    const claimed = claimedRoles(ws);
    for (const w of mesh) {
      for (const pane of w.panes) {
        for (const tab of pane.tabs) {
          if (!isAgentMember(tab)) continue;
          const former = formerRolesOf(tab.id).filter((f) => !claimed.includes(f.toLowerCase()));
          out.push({
            tabId: tab.id,
            role: roleName(tab.name),
            ...(former.length ? { formerRoles: former } : {}),
            cwd: getCwd(tab.id),
            purpose: tab.mesh_purpose ?? null,
            live: !!claudeStateStore.getState(tab.id),
            ...(mesh.length > 1 ? { workspace: w.name } : {}),
          });
        }
      }
    }
    return out;
  }

  /** Just this workspace's own members — the stage view, readiness, and what leaves the mesh
   *  when this one workspace is disabled or unlinked. */
  function localMembersOf(ws: Workspace): MeshMember[] {
    return membersOf(ws).filter((m) => ws.panes.some((p) => p.tabs.some((t) => t.id === m.tabId)));
  }

  /** Does this mesh workspace have an agent that ISN'T running right now? A tab with a
   *  persisted runtime (it WAS an agent) but no live agent-state has dropped — e.g. a
   *  resume that hasn't landed (or failed) after an app restart. Drives the auto re-check. */
  function hasUnreadyMembers(ws: Workspace): boolean {
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if ((tab.tab_type ?? 'terminal') !== 'terminal' || tab.service_id) continue; // stack services aren't agents
        if (!tab.runtime) continue; // never an agent → not expected in the mesh
        if (!claudeStateStore.getState(tab.id)) return true; // was an agent, not running now
      }
    }
    return false;
  }

  // Dialog-safe `/maiterm init` delivery for the headless initializeMesh pass (sibling of
  // MeshSetupModal's sendInit, minus its pending-UI supersede bookkeeping): a resumed agent may
  // be sitting at a startup dialog (e.g. "restore as is / compact first") that would swallow a
  // straight paste — so send a bare CR to answer it, wait for the PTY to go output-quiet
  // (compaction/thinking spinners repaint continuously), then deliver the init exactly once.
  //
  // Except Claude's workspace-trust dialog, where that CR confirms "No, exit". Each keystroke
  // waits for the screen to settle and looks first, since a just-started Claude may not have
  // drawn it yet (wake.ts, same rule).
  const INIT_QUIET_CAP_MS = 120_000;
  async function settleAndSendInit(tabId: string, ptyId: string) {
    const until = Date.now() + INIT_QUIET_CAP_MS;
    const safe = async () => {
      await waitQuiet(tabId, until);
      return !(await commands.trustDialogOpen(tabId));
    };
    if (!(await safe())) return;
    await commands.writeTerminal(ptyId, [0x0d]);
    if (!(await safe())) return;
    if (claudeStateStore.getState(tabId)) return; // re-registered on its own while settling
    await bracketedPasteSubmit(ptyId, '/maiterm init');
  }

  /** The router of this workspace's mesh. Safe inside a $derived read: a rebuild touches only
   *  plain maps, never $state. */
  function routerFor(wsId: string): MeshRouter | null {
    const ws = getWorkspace(wsId);
    if (!ws || !ws.bridge_all) return null;
    const key = meshKeyOf(ws);
    const mesh = meshWorkspacesOf(ws);
    const wsIds = mesh.map((w) => w.id);
    const cached = routers.get(key);
    if (cached && cached.wsIds.length === wsIds.length && cached.wsIds.every((id, i) => id === wsIds[i])) return cached.router;
    // New, or its workspaces changed. Keep every live topic's counters before any router goes.
    for (const e of routers.values()) {
      for (const t of e.router.all()) lastKnown.set(t.id, { ...t, participants: [...t.participants] });
    }
    // A router built over any of these workspaces is superseded (e.g. a workspace's own
    // router once it joins a group); its topics are in the mirror and `lastKnown`.
    for (const [k, e] of routers) if (k !== key && e.wsIds.some((id) => wsIds.includes(id))) routers.delete(k);
    const seed: MeshTopic[] = [];
    for (const w of mesh) {
      for (const t of w.mesh_topics ?? []) {
        topicHome.set(t.id, w.id);
        // The newer copy wins (every router mutation stamps updated_at): `lastKnown` can be
        // older than the mirror — a router dropped by a disable was flushed, not harvested —
        // and preferring it blindly would reopen a topic completed since.
        const live = lastKnown.get(t.id);
        seed.push(live && (live.updated_at > t.updated_at || (live.updated_at === t.updated_at && live.turn > t.turn)) ? live : t);
      }
    }
    const router = createMeshRouter({
      members: () => {
        const w = getWorkspace(wsId);
        return w ? membersOf(w) : [];
      },
      now: () => new Date().toISOString(),
      mintId: () => crypto.randomUUID(),
    });
    router.load(seed);
    routers.set(key, { router, wsIds });
    return router;
  }

  /** Flush this workspace's mesh registry: each topic to its home workspace (its owner's,
   *  once chosen), every workspace whose list changed. Updates the mirror too — a rebuild
   *  seeds from it. Not for $derived reads (it writes the mirror). */
  function persistTopics(wsId: string) {
    const ws = getWorkspace(wsId);
    const router = routerFor(wsId);
    if (!ws || !router) return;
    const mesh = meshWorkspacesOf(ws);
    const byWs = new Map<string, MeshTopic[]>(mesh.map((w) => [w.id, []]));
    for (const t of router.snapshot()) {
      let home = topicHome.get(t.id);
      if (!home || !byWs.has(home)) {
        const owner = workspaceOfTab(t.owner_tab_id);
        home = owner && byWs.has(owner.id) ? owner.id : ws.id;
        topicHome.set(t.id, home);
      }
      byWs.get(home)!.push(t);
    }
    for (const w of mesh) {
      const list = byWs.get(w.id)!;
      if (JSON.stringify(list) === JSON.stringify(w.mesh_topics ?? [])) continue;
      w.mesh_topics = list;
      commands.setWorkspaceMeshTopics(w.id, list).catch((e) =>
        logError(`agentMesh: failed to persist topics for ws ${w.id.slice(0, 8)}: ${e}`),
      );
    }
  }

  /** One workspace per mesh in this window — for passes that visit each registry once. */
  function meshRepresentatives(): string[] {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const ws of workspacesStore.workspaces) {
      if (!ws.bridge_all) continue;
      const key = meshKeyOf(ws);
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(ws.id);
    }
    return out;
  }

  /** Run the lifecycle sweep on one mesh's registry (see the TOPIC_* constants).
   *  NOT safe inside a $derived read (it bumps version) — call from rehydrate / the
   *  hourly interval, never from routerFor. */
  function sweepTopics(wsId: string) {
    const router = routerFor(wsId);
    if (!router) return;
    const { autoCompleted, expired } = router.sweep(Date.now(), {
      staleOpenMs: TOPIC_STALE_OPEN_MS,
      completedRetentionMs: TOPIC_COMPLETED_RETENTION_MS,
    });
    if (!autoCompleted.length && !expired.length) return;
    for (const t of autoCompleted) loopCtl.clear(t.id);
    for (const id of expired) loopCtl.clear(id);
    persistTopics(wsId);
    bump();
    logInfo(`agentMesh: topic sweep for ws ${wsId.slice(0, 8)} — auto-completed ${autoCompleted.length} stale open, expired ${expired.length} completed`);
  }

  // ─── Envelope (identity + topic stamped by maiTerm) ─────────────────────────

  function buildEnvelope(senderTabId: string, topic: MeshTopic, turn: number, message: string): string {
    const cwd = getCwd(senderTabId);
    const where = cwd ? `, working in ${cwd}` : '';
    const senderRole = roleForTab(senderTabId);
    const senderWs = workspaceOfTab(senderTabId);
    const inWs = senderWs && isLinked(senderWs) ? ` in workspace "${senderWs.name}"` : '';
    return (
      `⟦MESH⟧ Message from "${senderRole}"${inWs}${where} — a peer AI agent, NOT your human operator. [topic: ${topic.label}] [turn ${turn}]\n` +
      `Reply with the sendToBridgedAgent tool, tagging topic "${topic.id}". If this fully answers it, just stop — don't reply only to acknowledge.\n\n` +
      message
    );
  }

  function buildRenameNotice(oldRole: string, newRole: string): string {
    return (
      `⟦MESH⟧ Your human renamed your tab: on this mesh you are now "${newRole}" (formerly "${oldRole}"). ` +
      `Treat it as a clarification of your existing purpose, not a new assignment. Peers see your messages as from "${newRole}" ` +
      `and can still reach you by the old name. Don't announce this to anyone — just use the new name from here on and carry on.`
    );
  }

  const rosterLines = (peers: MeshMember[]) =>
    peers.map((p) => `  - "${p.role}"${p.workspace ? ` [${p.workspace}]` : ''}${p.purpose ? ` — ${p.purpose}` : p.cwd ? ` — ${p.cwd}` : ''}`).join('\n');

  function buildLinkNotice(workspaces: string[], newPeers: MeshMember[]): string {
    const names = workspaces.map((n) => `"${n}"`).join(', ');
    return (
      `⟦MESH⟧ Your human linked this mesh with workspace ${names}: its agents are now peers you can reach, like any other.\n` +
      (newPeers.length ? `New peers:\n${rosterLines(newPeers)}\n` : `(no agents there yet — they appear as they join; call listBridgedPeers anytime)\n`) +
      `Nothing to do now. Don't announce this to anyone — reach them only when your work needs to.`
    );
  }

  function buildUnlinkNotice(workspaces: string[], gone: MeshMember[]): string {
    const names = workspaces.map((n) => `"${n}"`).join(', ');
    const who = gone.length ? `: ${gone.map((p) => `"${p.role}"`).join(', ')} can no longer be reached` : '';
    return (
      `⟦MESH⟧ Your human unlinked workspace ${names} from this mesh${who}. ` +
      `Don't try to message them; threads you shared with them are closed to them. Don't announce this — just carry on.`
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

  function buildMeshOpener(member: MeshMember, peers: MeshMember[]): string {
    const where = member.cwd ? ` (working in ${member.cwd})` : '';
    const purpose = member.purpose?.trim();
    const roster = peers.length
      ? rosterLines(peers)
      : '  (no other agents yet — peers appear as they join; call listBridgedPeers anytime)';
    const spans = member.workspace
      ? ` This mesh spans several workspaces (peers are tagged with theirs); you sit in "${member.workspace}".`
      : '';
    return (
      `⟦MESH⟧ You've joined a Mesh Workspace as "${member.role}"${where}. Every agent here is a peer AI agent (NOT your human operator); you can talk to any of them.${spans}\n\n` +
      `Your purpose: ${purpose || '(your human will tell you — ask if unclear)'}\n\n` +
      `Peers you can reach:\n${roster}\n\n` +
      `How the mesh works:\n` +
      `  - Every message goes to ONE peer (no broadcast) and is tagged with a TOPIC. Start a thread by passing a short topic label to sendToBridgedAgent (you own it), or reply on an existing topic id from listTopics. Always tag a reply with the topic id shown in the incoming message.\n` +
      `  - Reusing a thread keeps context together; near-duplicate labels are deduped automatically.\n` +
      `  - When a thread's work is done, its OWNER calls completeTopic(id) so peers stop replying. Don't reply just to acknowledge.\n` +
      `  - Tools: listBridgedPeers, listTopics, startTopic, completeTopic, sendToBridgedAgent (recipient = a peer's role or handle; topic = id or new label).\n\n` +
      `Reaching your human: when you need a decision, or are blocked on something only the human can resolve, ASK with the AskUserQuestion tool — that is the ONE channel that reaches them (it also rings their phone via maiLink). Do NOT just print the question to the terminal, and do NOT write a "status" or "NEEDS DECISION" note — those are noise the human won't act on. If you have nothing the human must decide, stay silent.\n\n` +
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

  /** Hold a delivery slot for a member (idempotent), created ready iff its agent is live. */
  function ensureMember(tabId: string) {
    deliveryCtl.claim(tabId, OWNER, !!claudeStateStore.getState(tabId));
  }

  /** Let go of the mesh's hold; a bridge holding the same slot keeps it (and its queue). */
  function removeMember(tabId: string) {
    deliveryCtl.release(tabId, OWNER);
  }

  /** Prime a member on join: introduce it to the mesh once by injecting the opener. Idempotent
   *  within a session (`primed`) AND across restarts (persisted MESH_ONBOARDED_VAR) — a resumed
   *  agent already carries the opener in its transcript, so it's never re-introduced. No status
   *  note is created; the human-facing channel is the agent's native AskUserQuestion (see opener). */
  async function tryPrime(tabId: string) {
    if (primed.has(tabId)) return;
    const ws = meshWorkspaceForTab(tabId);
    if (!ws) return;
    const member = membersOf(ws).find((m) => m.tabId === tabId);
    if (!member || !member.live) return; // not a named, live agent yet — re-check on next Stop
    primed.add(tabId); // mark before the await so a racing event can't double-prime
    ensureMember(tabId);
    if (getVariables(tabId)?.get(MESH_ONBOARDED_VAR) === '1') { bump(); return; } // onboarded before
    const peers = membersOf(ws).filter((m) => m.tabId !== tabId);
    const status = await deliveryCtl.deliver(tabId, buildMeshOpener(member, peers));
    if (status === 'failed') { primed.delete(tabId); return; } // allow a retry on the next event
    await setVariable(tabId, MESH_ONBOARDED_VAR, '1');
    logInfo(`agentMesh: primed "${member.role}" (${tabId.slice(0, 8)}) into mesh "${ws.name}"`);
    bump();
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

  // ─── Enable / link / unlink ─────────────────────────────────────────────────

  async function setMeshEnabled(wsId: string, enabled: boolean) {
    const ws = getWorkspace(wsId);
    if (!ws) return;
    if (!enabled && ws.mesh_group) await unlinkMesh(wsId);
    if (!enabled) persistTopics(wsId); // flush live turn counts before the registry goes dormant
    await commands.setWorkspaceBridgeAll(wsId, enabled);
    ws.bridge_all = enabled;
    if (enabled) {
      const router = routerFor(wsId);
      if (router) for (const m of membersOf(ws)) { ensureMember(m.tabId); void tryPrime(m.tabId); }
    } else {
      // Leaving mesh mode: drop delivery entries for this ws's members (topics persist).
      for (const m of localMembersOf(ws)) {
        removeMember(m.tabId);
        primed.delete(m.tabId);
        void setVariable(m.tabId, MESH_ONBOARDED_VAR, null); // re-enabling should re-onboard
        void setVariable(m.tabId, MESH_FORMER_ROLES_VAR, null); // …under its current name only
      }
      routers.delete(meshKeyOf(ws));
    }
    bump();
    logInfo(`agentMesh: workspace ${wsId.slice(0, 8)} mesh ${enabled ? 'enabled' : 'disabled'}`);
  }

  const onboarded = (tabId: string) => getVariables(tabId)?.get(MESH_ONBOARDED_VAR) === '1';

  /** Tell each ONBOARDED member of `to` something (queued if busy). Members not yet onboarded
   *  get the opener later, which already carries the current roster. */
  function notifyMembers(to: MeshMember[], text: string) {
    for (const m of to) {
      if (!onboarded(m.tabId)) continue;
      ensureMember(m.tabId);
      void deliveryCtl.deliver(m.tabId, text);
    }
  }

  /** Link the meshes of two workspaces in this window into one (§17). Either side may be a
   *  mesh already — linked or not — or a plain workspace, which becomes a mesh. Two groups
   *  merge into one (in this window; a workspace of the absorbed group sitting in another
   *  window keeps the old id). Each side's onboarded agents are told who they can now reach. */
  async function linkMeshes(aId: string, bId: string): Promise<{ ok: true } | { error: string }> {
    const a = getWorkspace(aId);
    const b = getWorkspace(bId);
    if (!a || !b) return { error: 'Workspace not found.' };
    if (a.id === b.id) return { error: 'A workspace cannot link with itself.' };
    if (a.overlord || b.overlord) return { error: 'The Overlord workspace cannot join a mesh.' };
    const sideA = meshWorkspacesOf(a);
    const sideB = meshWorkspacesOf(b);
    if (sideA.some((w) => w.id === b.id)) return { error: 'These workspaces are already one mesh.' };
    const membersA = sideA.filter((w) => w.bridge_all).flatMap(localMembersOf);
    const membersB = sideB.filter((w) => w.bridge_all).flatMap(localMembersOf);
    for (const w of [...sideA, ...sideB]) if (w.bridge_all) persistTopics(w.id);
    const group = (a.bridge_all && a.mesh_group) || (b.bridge_all && b.mesh_group) || crypto.randomUUID();
    for (const w of [...sideA, ...sideB]) {
      if (w.mesh_group === group) continue;
      await commands.setWorkspaceMeshGroup(w.id, group);
      w.mesh_group = group;
    }
    // Enable the plain sides only after the link, and EVERY one of them before priming any
    // agent: an opener is built the moment it is primed, and a side primed while the other
    // was still plain would be introduced to a roster of itself — and, being outside both
    // notice lists below, never hear of the rest.
    const plain = [...sideA, ...sideB].filter((w) => !w.bridge_all);
    for (const w of plain) {
      await commands.setWorkspaceBridgeAll(w.id, true);
      w.bridge_all = true;
    }
    for (const w of plain) {
      for (const m of localMembersOf(w)) { ensureMember(m.tabId); void tryPrime(m.tabId); }
      logInfo(`agentMesh: workspace ${w.id.slice(0, 8)} mesh enabled (by link)`);
    }
    // The other side's members as the mesh now sees them (workspace-tagged).
    const joined = membersOf(a);
    const asJoined = (ms: MeshMember[]) => joined.filter((j) => ms.some((m) => m.tabId === j.tabId));
    const bAll = joined.filter((j) => sideB.some((w) => w.panes.some((p) => p.tabs.some((t) => t.id === j.tabId))));
    const aAll = joined.filter((j) => sideA.some((w) => w.panes.some((p) => p.tabs.some((t) => t.id === j.tabId))));
    notifyMembers(asJoined(membersA), buildLinkNotice(sideB.map((w) => w.name), bAll));
    notifyMembers(asJoined(membersB), buildLinkNotice(sideA.map((w) => w.name), aAll));
    bump();
    logInfo(`agentMesh: linked ${sideA.map((w) => w.name).join('+')} with ${sideB.map((w) => w.name).join('+')} (group ${group.slice(0, 8)})`);
    return { ok: true };
  }

  /** Take one workspace out of its linked mesh; it stays a mesh of its own. Its topics stay
   *  with it, the rest keep theirs (each topic lives on its owner's workspace). Both sides'
   *  onboarded agents are told who they lost. */
  async function unlinkMesh(wsId: string): Promise<void> {
    const ws = getWorkspace(wsId);
    if (!ws?.mesh_group) return;
    const rest = meshWorkspacesOf(ws).filter((w) => w.id !== ws.id);
    const mine = ws.bridge_all ? localMembersOf(ws) : [];
    const theirs = rest.flatMap(localMembersOf);
    if (ws.bridge_all) persistTopics(wsId);
    await commands.setWorkspaceMeshGroup(wsId, null);
    ws.mesh_group = null;
    if (ws.bridge_all && rest.length) {
      notifyMembers(mine, buildUnlinkNotice(rest.map((w) => w.name), theirs));
      notifyMembers(theirs, buildUnlinkNotice([ws.name], mine));
    }
    bump();
    logInfo(`agentMesh: unlinked ${ws.name} from ${rest.map((w) => w.name).join('+') || '(no workspace in this window)'}`);
  }

  // ─── Public API ─────────────────────────────────────────────────────────────

  return {
    get version() { return version; },

    getInternalSizes() {
      return { routers: routers.size, edges: edges.length };
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

    /** On load / activation of a mesh workspace (e.g. after an app restart), give auto-resume
     *  a few seconds to bring agents back, then — if any agent dropped — open the readiness
     *  modal so the human can wake/re-init it. Guarded to fire at most once per workspace per
     *  session, and only while that workspace is still the active one. */
    maybeAutoRecheck(wsId: string) {
      const ws = getWorkspace(wsId);
      // A suspended workspace has no agents to be ready — its PTYs are gone by
      // design. It can still be the active one: suspending the workspace you're
      // looking at leaves the pointer on it when there's nothing live to move to.
      // Guard before the latch so a later resume still gets its recheck.
      if (!ws?.bridge_all || ws.suspended || autoRechecked.has(wsId)) return;
      autoRechecked.add(wsId);
      setTimeout(() => {
        const w = getWorkspace(wsId);
        if (!w?.bridge_all || w.suspended) return;
        if (workspacesStore.activeWorkspaceId !== wsId) return; // user navigated away
        if (hasUnreadyMembers(w)) {
          window.dispatchEvent(new CustomEvent('open-mesh-setup', { detail: wsId }));
        }
      }, 5000);
    },

    /** Headless mesh readiness pass — maiLink's one-tap "Initialize all" (`mailink-mesh-init`
     *  event → here), the phone-driven equivalent of MeshSetupModal's triage with no UI. For
     *  every member that WAS an agent (persisted runtime) but has no live session, the process
     *  probe decides the remedy — still running (or a live ssh hop) → type `/maiterm init` into
     *  its PTY; process gone → replay the tab's auto-resume. Live members are untouched; tabs
     *  without a live PTY are skipped (a suspended workspace must be resumed first — the
     *  endpoint guards that). Members run concurrently so one slow tab doesn't serialize the
     *  rest; progress reaches the phone as each agent re-registers (dormant → active/idle). */
    initializeMesh(wsId: string) {
      const ws = getWorkspace(wsId);
      if (!ws?.bridge_all || ws.suspended) return;
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') !== 'terminal' || tab.service_id) continue; // stack services aren't agents
          if (!tab.runtime) continue; // never an agent — nothing to initialize
          if (claudeStateStore.getState(tab.id)) continue; // already live
          const inst = terminalsStore.get(tab.id);
          if (!inst) continue; // no live PTY — can't reach it headlessly
          const tabId = tab.id;
          const ptyId = inst.ptyId;
          const hasResume = !!tab.auto_resume_command;
          void (async () => {
            try {
              const l = await commands.getAgentLiveness(ptyId);
              if (l.agent_running || l.ssh_foreground) {
                await settleAndSendInit(tabId, ptyId);
              } else if (hasResume) {
                await replayAutoResume(tabId);
              } else {
                logInfo(`mesh init (maiLink): ${tabId.slice(0, 8)} dropped with no auto-resume — skipped`);
              }
            } catch (e) {
              logError(`mesh init (maiLink) failed for ${tabId.slice(0, 8)}: ${e}`);
            }
          })();
        }
      }
    },

    /** Toggle a workspace into / out of mesh mode (persisted). Disabling a linked workspace
     *  unlinks it first, so its peers are told they lost it. */
    setMeshEnabled,

    /** Is this workspace's mesh linked with another workspace in this window? */
    isLinkedMesh(wsId: string): boolean {
      void version;
      const ws = getWorkspace(wsId);
      return !!ws?.bridge_all && isLinked(ws);
    },

    /** The workspaces making up this workspace's mesh (itself first-class among them). */
    linkedWorkspaces(wsId: string): { id: string; name: string }[] {
      void version;
      const ws = getWorkspace(wsId);
      return ws?.bridge_all ? meshWorkspacesOf(ws).map((w) => ({ id: w.id, name: w.name })) : [];
    },

    /** Workspaces this one could link its mesh with: any other non-Overlord workspace in this
     *  window not already on the same mesh. A non-mesh pick becomes a mesh by linking. */
    linkCandidates(wsId: string): { id: string; name: string; mesh: boolean }[] {
      void version;
      const ws = getWorkspace(wsId);
      if (!ws || ws.overlord) return [];
      const same = new Set(ws.bridge_all ? meshWorkspacesOf(ws).map((w) => w.id) : [ws.id]);
      return workspacesStore.workspaces
        .filter((w) => !same.has(w.id) && !w.overlord)
        .map((w) => ({ id: w.id, name: w.name, mesh: !!w.bridge_all }));
    },

    linkMeshes,
    unlinkMesh,

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

    /** This workspace's OWN members — the stage view's filmstrip. On a linked mesh the
     *  whole roster is `rosterForTab` / `statusBoard`. */
    rosterForWorkspace(wsId: string): MeshMember[] {
      void version;
      const ws = getWorkspace(wsId);
      return ws && ws.bridge_all ? localMembersOf(ws) : [];
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

    /** The status board for the cockpit: each member with its live claude state and whether it
     *  currently needs the human. "Needs you" is the agent's native awaiting-human-input state
     *  (AskUserQuestion / permission) — the single deterministic signal, no status-note parsing. */
    statusBoard(wsId: string) {
      void version;
      const ws = getWorkspace(wsId);
      if (!ws || !ws.bridge_all) return [];
      return membersOf(ws).map((m) => {
        const cs = claudeStateStore.getState(m.tabId);
        const needsInput = !!cs && getAdapter(workspacesStore.getTabRuntime(m.tabId)).isAwaitingHumanInput(cs);
        return {
          tabId: m.tabId,
          role: m.role,
          workspace: m.workspace ?? null,
          cwd: m.cwd,
          purpose: m.purpose,
          live: m.live,
          claudeState: cs?.state ?? null,
          needsInput,
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
      const memberIds = new Set(ws ? localMembersOf(ws).map((m) => m.tabId) : []);
      return { left: s.left && memberIds.has(s.left) ? s.left : null, right: s.right && memberIds.has(s.right) ? s.right : null };
    },

    /** Turn the stage layout on/off for a mesh workspace; seeds the two slots on first on. */
    toggleStageView(wsId: string) {
      const ws = getWorkspace(wsId);
      if (!ws || !ws.bridge_all) return;
      const s = stage.get(wsId) ?? { active: false, left: null, right: null };
      s.active = !s.active;
      if (s.active) {
        const members = localMembersOf(ws).map((m) => m.tabId);
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
     *  ALL members render live in stage view (stage at scale 1, filmstrip CSS-scaled). The
     *  stage is per workspace even on a linked mesh, so this asks the tab's own workspace. */
    isMeshMemberTab(tabId: string): boolean {
      void version;
      const ws = meshWorkspaceForTab(tabId);
      return !!ws && localMembersOf(ws).some((m) => m.tabId === tabId);
    },

    // ─── MCP tool: listBridgedPeers ───────────────────────────────────────────
    listPeers(tabId: string) {
      const ws = meshWorkspaceForTab(tabId);
      if (!ws) {
        return { error: 'You are not in a mesh workspace. listBridgedPeers only applies inside a Mesh Workspace.' };
      }
      const peers = membersOf(ws)
        .filter((m) => m.tabId !== tabId)
        .map((m) => ({ handle: m.tabId, role: m.role, ...(m.workspace ? { workspace: m.workspace } : {}), cwd: m.cwd, purpose: m.purpose, live: m.live }));
      const linked = meshWorkspacesOf(ws);
      return { workspace: ws.name, ...(linked.length > 1 ? { linkedWorkspaces: linked.map((w) => w.name) } : {}), you: tabId, peers };
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

    // ─── Cockpit: human topic deletion (✕ / "Clear done") ──────────────────────

    /** Human hard-deletes a topic outright. Silent — no agent notice; a late reply tagged
     *  with the dead id errors at the send boundary instead of minting a junk topic. */
    deleteTopic(topicId: string): { success: true } | { error: string } {
      const ctx = findTopicById(topicId);
      if (!ctx) return { error: `Topic not found: ${topicId}` };
      routerFor(ctx.ws.id)?.remove(topicId);
      loopCtl.clear(topicId);
      persistTopics(ctx.ws.id);
      bump();
      logInfo(`agentMesh: topic ${topicId.slice(0, 8)} "${ctx.topic.label}" deleted by human`);
      return { success: true };
    },

    /** Human clears every completed topic of a workspace in one click. */
    clearCompletedTopics(wsId: string): number {
      const router = routerFor(wsId);
      if (!router) return 0;
      const removed = router.clearCompleted();
      if (removed.length) {
        for (const id of removed) loopCtl.clear(id);
        persistTopics(wsId);
        bump();
        logInfo(`agentMesh: cleared ${removed.length} completed topic(s) for ws ${wsId.slice(0, 8)}`);
      }
      return removed.length;
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

    /** The in-memory mirror took a new tab name (workspacesStore._applyTabRename). The roster
     *  is derived, so listBridgedPeers / the cockpit / envelopes already show the new name —
     *  what goes stale is what the AGENTS were told. Three cases:
     *    • an onboarded member whose role changed → it is told its new name once (a short
     *      prompt, queued if busy) and the old name is recorded as a former role, so peers
     *      whose transcripts still say "Bob" keep routing and learn "Billing API" from the
     *      send result — no turn spent on any peer.
     *    • a tab that became a member BY being named → primed now, not on its next Stop.
     *    • a cosmetic change (casing, glyph) or a tab outside any mesh → nothing to tell. */
    async handleTabRenamed(tabId: string, prev: { name: string; custom_name: boolean }) {
      const ws = meshWorkspaceForTab(tabId);
      if (!ws) return;
      const onboarded = getVariables(tabId)?.get(MESH_ONBOARDED_VAR) === '1';
      const member = membersOf(ws).find((m) => m.tabId === tabId);
      if (!member) {
        // Not an agent, or its name was RESET to a default (custom_name → false), which drops it
        // from the derived roster. If it was an onboarded member, remember the name it was known
        // by: the re-name that follows a reset arrives with a non-custom `prev`, and this is the
        // only way that re-name can still tell the agent and its peers what changed.
        if (onboarded && prev.custom_name) await updateFormerRoles(tabId, '', roleName(prev.name));
        return;
      }
      if (!onboarded) { void tryPrime(tabId); return; } // joined by being named: the opener carries the right name
      // The name it was known by: the previous custom name, or — after a reset — the last one recorded.
      const known = formerRolesOf(tabId);
      const oldRole = prev.custom_name ? roleName(prev.name) : known[known.length - 1];
      if (!oldRole) return;
      if (sameRole(oldRole, member.role)) { await updateFormerRoles(tabId, member.role, null); return; } // cosmetic, or back to a known name
      await updateFormerRoles(tabId, member.role, oldRole);
      ensureMember(tabId);
      const status = await deliveryCtl.deliver(tabId, buildRenameNotice(oldRole, member.role));
      logInfo(`agentMesh: "${oldRole}" → "${member.role}" (${tabId.slice(0, 8)}) in mesh "${ws.name}" — notice ${status}`);
      bump();
    },

    /** A tab is being closed — drop its mesh delivery slot. Topics persist (it may reopen). */
    handleTabClosed(tabId: string) {
      removeMember(tabId);
      primed.delete(tabId);
      for (const s of stage.values()) { if (s.left === tabId) s.left = null; if (s.right === tabId) s.right = null; }
      bump();
    },

    /** Tab reload minted a new id — carry the delivery queue + priming state across. The
     *  delivery remap is a no-op when the bridge store already moved the shared slot. */
    remapTab(oldTabId: string, newTabId: string) {
      if (oldTabId === newTabId) return;
      deliveryCtl.remap(oldTabId, newTabId);
      if (primed.has(oldTabId)) { primed.delete(oldTabId); primed.add(newTabId); }
      for (const s of stage.values()) { if (s.left === oldTabId) s.left = newTabId; if (s.right === oldTabId) s.right = newTabId; }
      bump();
    },

    async init() {
      // A mesh member's agent came online (fresh start or resume) → it can receive now.
      const u1 = await listen<{ tab_id: string | null; session_id: string }>('agent-init-session', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !meshWorkspaceForTab(tabId)) return;
        deliveryCtl.markReadyOrCreate(tabId, OWNER);
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

      // Hourly topic-lifecycle sweep for long-running sessions (rehydrate covers app start).
      const sweepInterval = setInterval(() => {
        for (const wsId of meshRepresentatives()) sweepTopics(wsId);
      }, TOPIC_SWEEP_INTERVAL_MS);
      unlisteners.push(() => clearInterval(sweepInterval));
    },

    /** Rebuild routers (and their topic registries) from persisted state after load. */
    rehydrate() {
      let count = 0;
      for (const wsId of meshRepresentatives()) {
        const ws = getWorkspace(wsId);
        if (!ws || !routerFor(wsId)) continue;
        for (const m of membersOf(ws)) ensureMember(m.tabId);
        sweepTopics(wsId);
        count++;
      }
      if (count) { bump(); logInfo(`agentMesh: rehydrated ${count} mesh workspace(s)`); }
    },

    destroy() {
      for (const u of unlisteners) u();
      unlisteners.length = 0;
      // The shared delivery controller is torn down once by +layout, not per store.
      loopCtl.reset();
      routers.clear();
      topicHome.clear();
      lastKnown.clear();
      primed.clear();
      stage.clear();
      autoRechecked.clear();
      edges.length = 0;
    },
  };
}

export const agentMeshStore = createAgentMeshStore();
