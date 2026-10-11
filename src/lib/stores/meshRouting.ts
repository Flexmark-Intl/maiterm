/**
 * Mesh routing core — the pure addressing + topic-registry logic for a Mesh Workspace
 * (docs/mesh-workspace.md). No Svelte/Tauri imports: the live roster, the clock, and the
 * id minter are injected, so this is unit-testable in isolation (meshRouting.test.ts).
 *
 * Two responsibilities, both safety-critical:
 *
 *   1. RECIPIENT RESOLUTION (Codex #2). Routing keys off a STABLE handle (the tabId), never
 *      the human-editable role name. A `recipient` arg resolves: exact tabId first, then a
 *      UNIQUE case-insensitive role match. An ambiguous or unknown name is a hard error WITH
 *      the roster — never a silent misroute (16.5 critical gap). Self-sends are rejected.
 *
 *   2. TOPIC REGISTRY (D3, Codex #7/#9). Topics are first-class objects, deduped by a
 *      normalized label (so "Auth Bug" / "auth_bug" / "auth-bug" are one thread). A send's
 *      `topic` accepts an existing id or a new label (create-on-first-send, sender becomes
 *      owner). Only the owner — or the human — can complete a topic; a completed topic
 *      REJECTS further sends at this boundary (not by trusting the model).
 *
 * The store (agentMesh.svelte.ts) wires the deps to live workspace state and flushes the
 * registry to the backend after structural changes.
 */

import type { MeshTopic } from '$lib/tauri/types';

/** A reachable peer on the mesh. `tabId` is the routing handle; `role` is display only. */
export interface MeshMember {
  /** Stable, maiTerm-minted routing handle. */
  tabId: string;
  /** Human-given descriptive name — the addressable label, display only (never the key). On a
   *  LINKED mesh it is `Workspace/Tab`: tab names repeat across workspaces, and a name an agent
   *  reads must be one it can type back (docs/mesh-workspace.md §17). */
  role: string;
  /** The bare tab name, set only when `role` is qualified. Still addressable while no other
   *  workspace on the mesh has a tab by that name — the common case, and what agents linked
   *  before names were qualified have in their transcripts. */
  shortRole?: string;
  /** Names this member was introduced to peers under BEFORE its current role (the human renamed
   *  the tab after it was onboarded). Peers whose transcripts still carry an old name keep
   *  routing without spending a turn on a "no such peer" error. Never a current role. */
  formerRoles?: string[];
  cwd: string | null;
  purpose: string | null;
  /** Has a live agent session right now (vs dormant/booting). */
  live: boolean;
  /** The workspace this member sits in — set only on a LINKED mesh (several workspaces,
   *  docs/mesh-workspace.md §17), where it tells two same-named roles apart. */
  workspace?: string;
}

/** A tab's addressable role: its display name with any legacy bridge glyph stripped. The one
 *  definition shared by the mesh, the 1:1 bridge and the setup modal, so all label an agent
 *  identically. */
export function roleName(tabName: string): string {
  return tabName.replace(/^[⇄↔→⌗]\s*/u, '').trim() || 'agent';
}

/** Per-tab trigger variables the mesh uses as persisted identity markers (no new Tab field):
 *  "this agent has been introduced" and "the names it was introduced under before a rename".
 *  They describe ONE agent's history, so a DUPLICATE must not inherit them — a clone is a new
 *  member that needs its own opener, and a shared former name would make the old name
 *  ambiguous. A RELOAD keeps them (same agent, new id) via Rust `carry_tab_state_on_reload`. */
export const MESH_ONBOARDED_VAR = 'meshOnboarded';
export const MESH_FORMER_ROLES_VAR = 'meshFormerRoles';
export const MESH_IDENTITY_VARS: readonly string[] = [MESH_ONBOARDED_VAR, MESH_FORMER_ROLES_VAR];

export interface MeshRouterDeps {
  /** Current roster (the store derives this from workspace membership). */
  members(): MeshMember[];
  /** ISO-8601 now (injected for deterministic tests). */
  now(): string;
  /** Mint a stable topic id (injected for deterministic tests). */
  mintId(): string;
}

export type RecipientResolution =
  /** `viaFormerRole` is set when the sender addressed a peer by a name it no longer has —
   *  the send path uses it to teach the sender the current role in the tool result. */
  | { ok: true; tabId: string; role: string; viaFormerRole?: string }
  | { ok: false; error: string };

export type TopicResolution =
  | { ok: true; topic: MeshTopic; created: boolean }
  | { ok: false; error: string };

export type CompleteResolution =
  | { ok: true; topic: MeshTopic; participants: string[]; alreadyComplete: boolean }
  | { ok: false; error: string };

/**
 * Mirror of Rust `MeshTopic::normalize_label`: trim, lowercase, split on any run of
 * whitespace / `_` / `-`, drop empties, rejoin with `-`. MUST stay in lockstep with the
 * Rust impl so a topic created in one layer dedups against the other.
 */
/** UUID shape — how topic ids are minted in production (crypto.randomUUID). */
const LOOKS_LIKE_TOPIC_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function normalizeLabel(label: string): string {
  return label
    .trim()
    .toLowerCase()
    .split(/[\s_-]+/)
    .filter(Boolean)
    .join('-');
}

/** Would the mesh resolve `name` to a member — by handle, role, bare tab name, former name or a
 *  stale `Workspace/` prefix? The same rules as `resolveRecipient`, minus ambiguity: the 1:1
 *  bridge's dispatch asks it so a name the mesh knows is never sent over the bridge instead. */
export function meshKnowsName(members: MeshMember[], name: string): boolean {
  const lc = name.trim().toLowerCase();
  if (!lc) return false;
  return members.some((m) =>
    m.tabId === name.trim() ||
    m.role.trim().toLowerCase() === lc ||
    m.shortRole?.trim().toLowerCase() === lc ||
    (m.formerRoles ?? []).some((f) => f.trim().toLowerCase() === lc) ||
    (!!m.shortRole && lc.endsWith(`/${m.shortRole.trim().toLowerCase()}`)),
  );
}

export function createMeshRouter(deps: MeshRouterDeps) {
  // In-memory authoritative registry (seeded from persisted state via load()).
  let topics: MeshTopic[] = [];

  function rosterHint(): string {
    const names = deps.members().map((m) => `"${m.role}"`).join(', ');
    return names ? ` Known peers: ${names}.` : ' There are no other agents in this mesh yet.';
  }

  // ─── Recipient resolution ───────────────────────────────────────────────────

  function self(m: MeshMember, senderTabId: string, via?: string): RecipientResolution {
    if (m.tabId === senderTabId) return { ok: false, error: 'Cannot send to yourself.' };
    return { ok: true, tabId: m.tabId, role: m.role, ...(via ? { viaFormerRole: via } : {}) };
  }

  function resolveRecipient(senderTabId: string, recipientArg: string | undefined): RecipientResolution {
    const arg = (recipientArg ?? '').trim();
    const members = deps.members();
    const others = members.filter((m) => m.tabId !== senderTabId);

    if (!arg) {
      // Convenience: in a 2-agent mesh the recipient is unambiguous, so allow omission.
      // With 0 or 2+ peers it must be explicit (never guess across multiple peers).
      if (others.length === 1) return { ok: true, tabId: others[0].tabId, role: others[0].role };
      return { ok: false, error: `recipient is required in a mesh workspace.${rosterHint()}` };
    }

    // 1. Exact tabId handle (the canonical, always-unambiguous key).
    const byId = members.find((m) => m.tabId === arg);
    if (byId) return self(byId, senderTabId);

    // 2. Unique case-insensitive role match. Ambiguity is an error, never a guess.
    const lc = arg.toLowerCase();
    const byRole = members.filter((m) => m.role.trim().toLowerCase() === lc);
    if (byRole.length === 1) return self(byRole[0], senderTabId);
    if (byRole.length > 1) {
      return {
        ok: false,
        error: `Role "${arg}" is ambiguous — ${byRole.length} peers share that name. Address by tabId handle instead (see listBridgedPeers).`,
      };
    }

    // 3. A bare tab name on a linked mesh, where roles are `Workspace/Tab`: unique → that peer;
    //    the same tab name in two workspaces → an error naming both full names, never a guess.
    const byShort = members.filter((m) => m.shortRole?.trim().toLowerCase() === lc);
    if (byShort.length === 1) return self(byShort[0], senderTabId);
    if (byShort.length > 1) {
      return {
        ok: false,
        error: `"${arg}" is a tab in ${byShort.length} linked workspaces: ${byShort.map((m) => `"${m.role}"`).join(', ')}. Address one by its full name.`,
      };
    }

    // 4. Unique match on a FORMER role — the peer was renamed after this sender learned its
    //    name. A current role always shadows a former one (steps 2–3 ran first); ambiguity across
    //    former names is still an error, never a guess.
    const byFormer = members.filter((m) => (m.formerRoles ?? []).some((f) => f.trim().toLowerCase() === lc));
    if (byFormer.length === 1) return self(byFormer[0], senderTabId, arg);
    if (byFormer.length > 1) {
      return {
        ok: false,
        error: `"${arg}" is a former name of ${byFormer.length} peers. Address by current role or tabId handle instead (see listBridgedPeers).`,
      };
    }

    // 5. `Workspace/Tab` whose workspace part is stale — the workspace was renamed, or the sender
    //    misremembers it — matched by its whole tab name at the end, never by splitting at a `/`
    //    (either name may hold one). Unique only; the result teaches the current name.
    const bySuffix = members.filter((m) => m.shortRole && lc.endsWith(`/${m.shortRole.trim().toLowerCase()}`));
    if (bySuffix.length === 1) return self(bySuffix[0], senderTabId, arg);
    if (bySuffix.length > 1) {
      return {
        ok: false,
        error: `No peer is named "${arg}"; its tab name matches ${bySuffix.map((m) => `"${m.role}"`).join(', ')}. Address one by its full name.`,
      };
    }
    return { ok: false, error: `No peer named "${arg}" in this mesh.${rosterHint()}` };
  }

  // ─── Topic registry ─────────────────────────────────────────────────────────

  function makeTopic(label: string, normalized: string, ownerTabId: string): MeshTopic {
    const now = deps.now();
    return {
      id: deps.mintId(),
      label: label.trim(),
      normalized_label: normalized,
      owner_tab_id: ownerTabId,
      state: 'open',
      participants: [ownerTabId],
      turn: 0,
      created_at: now,
      updated_at: now,
    };
  }

  /** Resolve the `topic` arg of a send: an existing topic id, or a new/existing label
   *  (create-on-first-send with normalized dedup). Completed topics reject. */
  function resolveTopicForSend(senderTabId: string, topicArg: string | undefined): TopicResolution {
    const arg = (topicArg ?? '').trim();
    if (!arg) {
      return { ok: false, error: 'topic is required: pass an existing topic id (see listTopics) or a short new label to start a thread.' };
    }
    const byId = topics.find((t) => t.id === arg);
    if (byId) {
      if (byId.state === 'complete') {
        return { ok: false, error: `Topic "${byId.label}" is complete — start a new topic to continue this line of work.` };
      }
      return { ok: true, topic: byId, created: false };
    }
    // A UUID-shaped arg is a dangling topic id (completed-and-expired, or human-deleted),
    // not a label — creating a topic literally labeled with the UUID accretes junk threads.
    if (LOOKS_LIKE_TOPIC_ID.test(arg)) {
      return { ok: false, error: `Topic id ${arg} not found — it may have been completed and cleared. Reply on an open topic from listTopics, or start a new one with a short descriptive label.` };
    }
    const norm = normalizeLabel(arg);
    if (!norm) return { ok: false, error: 'topic label is empty after normalization.' };
    const existing = topics.find((t) => t.state === 'open' && t.normalized_label === norm);
    if (existing) return { ok: true, topic: existing, created: false };
    const topic = makeTopic(arg, norm, senderTabId);
    topics.push(topic);
    return { ok: true, topic, created: true };
  }

  /** Explicit startTopic tool: reuse an open dedup match or mint a new owned topic. */
  function startTopic(ownerTabId: string, label: string): TopicResolution {
    const arg = (label ?? '').trim();
    if (!arg) return { ok: false, error: 'label is required to start a topic.' };
    const norm = normalizeLabel(arg);
    if (!norm) return { ok: false, error: 'label is empty after normalization.' };
    const existing = topics.find((t) => t.state === 'open' && t.normalized_label === norm);
    if (existing) return { ok: true, topic: existing, created: false };
    const topic = makeTopic(arg, norm, ownerTabId);
    topics.push(topic);
    return { ok: true, topic, created: true };
  }

  /** Owner-or-human completes a topic. Idempotent. Returns participants to signal. */
  function completeTopic(byTabId: string | null, topicId: string, isHuman: boolean): CompleteResolution {
    const topic = topics.find((t) => t.id === topicId);
    if (!topic) return { ok: false, error: `Topic not found: ${topicId}` };
    if (!isHuman && topic.owner_tab_id !== byTabId) {
      return { ok: false, error: `Only the topic owner — or the human — can complete "${topic.label}".` };
    }
    if (topic.state === 'complete') {
      return { ok: true, topic, participants: [...topic.participants], alreadyComplete: true };
    }
    topic.state = 'complete';
    topic.updated_at = deps.now();
    return { ok: true, topic, participants: [...topic.participants], alreadyComplete: false };
  }

  /** Add a tab to a topic's participant set. Returns true if it was newly added. */
  function recordParticipant(topicId: string, tabId: string): boolean {
    const topic = topics.find((t) => t.id === topicId);
    if (!topic) return false;
    if (topic.participants.includes(tabId)) return false;
    topic.participants.push(tabId);
    topic.updated_at = deps.now();
    return true;
  }

  /** Increment a topic's turn counter (drives the soft cap + the map edge weight). */
  function bumpTurn(topicId: string): number {
    const topic = topics.find((t) => t.id === topicId);
    if (!topic) return 0;
    topic.turn += 1;
    topic.updated_at = deps.now();
    return topic.turn;
  }

  /** Lifecycle sweep: auto-complete open topics idle past `staleOpenMs` (agents rarely call
   *  completeTopic, so abandoned threads otherwise stay open forever), and hard-delete
   *  completed topics idle past `completedRetentionMs` (long enough for the human to see the
   *  closure dimmed in the cockpit). An auto-completed topic gets a fresh `updated_at`, so it
   *  rides out the retention window before expiring — never both transitions in one pass. */
  function sweep(nowMs: number, opts: { staleOpenMs: number; completedRetentionMs: number }): { autoCompleted: MeshTopic[]; expired: string[] } {
    const autoCompleted: MeshTopic[] = [];
    const expired: string[] = [];
    const keep: MeshTopic[] = [];
    for (const t of topics) {
      const idleMs = nowMs - (Date.parse(t.updated_at) || nowMs);
      if (t.state === 'open' && idleMs > opts.staleOpenMs) {
        t.state = 'complete';
        t.updated_at = deps.now();
        autoCompleted.push(t);
        keep.push(t);
      } else if (t.state === 'complete' && idleMs > opts.completedRetentionMs) {
        expired.push(t.id);
      } else {
        keep.push(t);
      }
    }
    topics = keep;
    return { autoCompleted, expired };
  }

  return {
    /** Seed the registry from persisted workspace state. */
    load(persisted: MeshTopic[]) {
      topics = (persisted ?? []).map((t) => ({ ...t, participants: [...(t.participants ?? [])] }));
    },
    /** Snapshot for persistence (caller flushes to the backend). */
    snapshot(): MeshTopic[] {
      return topics.map((t) => ({ ...t, participants: [...t.participants] }));
    },
    all(): MeshTopic[] {
      return topics;
    },
    open(): MeshTopic[] {
      return topics.filter((t) => t.state === 'open');
    },
    get(id: string): MeshTopic | null {
      return topics.find((t) => t.id === id) ?? null;
    },
    resolveRecipient,
    resolveTopicForSend,
    startTopic,
    completeTopic,
    recordParticipant,
    bumpTurn,
    sweep,
    /** Hard-delete a topic (human ✕ in the cockpit). Returns the removed topic, or null. */
    remove(topicId: string): MeshTopic | null {
      const i = topics.findIndex((t) => t.id === topicId);
      return i === -1 ? null : topics.splice(i, 1)[0];
    },
    /** Hard-delete every completed topic (human "Clear done"). Returns the removed ids. */
    clearCompleted(): string[] {
      const removed = topics.filter((t) => t.state === 'complete').map((t) => t.id);
      if (removed.length) topics = topics.filter((t) => t.state !== 'complete');
      return removed;
    },
    /** Drop topics owned by / participated in by tabs that no longer exist, keeping the
     *  registry from accreting dead threads. `liveTabIds` is the current member set. */
    pruneFor(liveTabIds: Set<string>) {
      const before = topics.length;
      topics = topics.filter((t) => liveTabIds.has(t.owner_tab_id) || t.participants.some((p) => liveTabIds.has(p)));
      return before - topics.length;
    },
  };
}

export type MeshRouter = ReturnType<typeof createMeshRouter>;
