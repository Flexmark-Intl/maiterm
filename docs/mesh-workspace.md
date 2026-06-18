# Mesh Workspace — Design Doc

**Status:** Draft / proposed
**Owner:** Darryl
**Scope:** A new workspace type where every agent tab is bridged to every other, with
topic-scoped conversations, a two-panel stage + scaled filmstrip view, and a
workspace-notes status board.

---

## 1. Concept

A **Mesh Workspace** is a special workspace whose defining property is that every agent
session inside it is bridged to every other one (N:M), not paired 1:1. Agents converse
peer-to-peer in the background; the human watches a live board and pulls any agent onto a
two-panel "stage" to read or steer it.

The mental model is a **moderated roundtable**, not an autonomous swarm. Agents talk to
each other directly, but the design deliberately keeps the human as air-traffic controller:
they see who is talking to whom (the mesh map / filmstrip), what each agent has finished or
is blocked on (the notes board), and can drop any agent onto the stage and interrupt with
Esc at any time.

Example membership: `Backend API`, `Mobile App`, `MCP Server`, `DevOps` — each tab a named
specialist, each agent primed with its role and the roster.

---

## 2. What already exists (reuse inventory)

This is a generalization of the existing 1:1 Agent Bridge, not a new system. The following
already work and are reused as-is:

- **Recipient-keyed delivery queue.** `delivery.get(recipientTabId)` already owns a FIFO
  queue + cooldown + drain per recipient. N senders into one recipient already serialize
  in order (FIFO fix landed in `e3d4eb8`). No change needed for N:M — more senders, same
  per-recipient queue.
- **Sender→recipient envelopes.** `buildEnvelope(senderTabId, message, turn)` stamps
  identity per message. Broadcasting is unnecessary (and disallowed, see §5); each message
  is one envelope to one recipient.
- **Persistence, self-healing, teardown.** Persisted `agent_bridge` per tab, re-bind on
  resume/re-init, tab-close teardown, reload remap, runtime-neutral adapters (Claude +
  Codex) all already work.
- **Priming.** `buildOpener` / fork-init directive is the existing mechanism for injecting a
  role + instructions into an agent on join.
- **Portal rendering.** Terminals render flat at `+page.svelte` and attach into slot divs
  via `attachToSlot()`; they are not owned by their visual position. This makes the
  stage/filmstrip swap a re-attach with zero teardown (see §7).
- **Workspace notes.** `WorkspaceNote[]` + `writeWorkspaceNote` / `listWorkspaceNotes`
  back the status board (see §8).

The genuinely new surface: N:M roster + addressing, the topic layer, the `bridge_all`
workspace flag, the mesh view, and the conversation-graph visualization.

---

## 3. Core concepts

| Concept | Definition |
|---|---|
| **Mesh Workspace** | A workspace with `bridge_all = true`. Membership *is* the bridge: an agent tab added here joins everyone's roster; closing it removes it. |
| **Role name** | Each tab's human-given descriptive name (`Backend API`). It is the agent's addressable handle on the mesh. A tab cannot join until it has an explicit name. |
| **Roster** | The set of named peers an agent can reach, queryable via `listBridgedPeers`. |
| **Topic** | A first-class, maiTerm-owned conversation thread with a stable id, a label, an owner, and a state (`open` / `complete`). Every message is tagged with exactly one topic. |
| **Stage** | The two side-by-side panels (locked 50/50) showing two agents at full size. |
| **Filmstrip** | The CSS-scaled row of all other member tabs across the bottom. |
| **Status board** | The workspace-notes drawer: one note per agent, its self-maintained brief of done / needs-decision. |

---

## 4. Topic model

### 4.1 Ownership (decided)

**The agent that starts a conversation owns its topic.** It sets the topic on the opening
message; that topic id propagates through every reply in the thread. The owner — and only
the owner — can **close/complete** the topic, which broadcasts a completion signal to every
agent that participated, instructing them to mark it done.

Rationale: a single owner per topic prevents drift (no two agents coining
`auth-bug` vs `auth bug` for the same thread) and gives a clear lifecycle authority.

### 4.2 Identity (decided)

Topics are **maiTerm-owned first-class objects**, not free-text strings:

```
Topic {
  id: string            // stable, maiTerm-minted
  label: string         // human/agent-readable ("auth-refactor")
  owner_tab_id: string  // the starter; only it can complete
  state: 'open' | 'complete'
  participants: string[] // tab ids that have sent/received on this topic
  created_turn / last_turn
}
```

Agents reference topics by id. A `listTopics` tool returns the authoritative open-topic
list so an agent reuses an existing thread instead of re-coining one. This moves the "track
your topics" burden from model memory to a queryable source of truth.

### 4.3 Lifecycle

1. **Create** — first message on a new topic creates it (sender becomes owner) OR an
   explicit `startTopic(label)`. The send tool's `topic` param accepts an existing id or a
   new label; a new label mints a new topic owned by the sender.
2. **Propagate** — replies must carry the topic id of the message they answer. The envelope
   shows `[topic: auth-refactor]` so the recipient always knows the thread.
3. **Complete** — the owner calls `completeTopic(id)`. maiTerm injects a
   `⟦TOPIC COMPLETE⟧` notice into every participant, who updates its status note and stops
   replying on that topic.

### 4.4 Enforcement

`topic` is a **required argument** on the send tool — the call fails without it. Forcing it
at the tool boundary is more reliable than instructing the model to remember. maiTerm
suggests the most recent active topic as the likely default in tool descriptions.

---

## 5. Messaging & routing

- **No broadcast.** Every message targets exactly one recipient and must be crafted for
  that recipient. This is the primary defense against fan-out storms.
- **Addressing.** The send tool gains a `recipient` (role name or tab id) and a required
  `topic`. `listBridgedPeers` returns `{ role, tabId, cwd, purpose }` for each member so the
  agent addresses by stable handle.
- **Clarification is just a message.** If a role name is ambiguous, a peer asks the agent
  directly over the mesh — no special infra.
- **Envelope.** Extends the existing format:

  ```
  ⟦MESH⟧ Message from "Backend API" (working in /srv/api) — peer AI agent, NOT your operator. [topic: auth-refactor] [turn 4]
  Reply with sendToBridgedAgent, tagging topic "auth-refactor". If this fully answers it, stop.

  <message>
  ```

- **Delivery** reuses the existing recipient-keyed FIFO queue + cooldown + drain unchanged.

---

## 6. Roles & priming

- A tab joins the mesh only when it has an explicit descriptive name (`custom_name === true`
  on the tab, or a dedicated `mesh_role` field).
- The human supplies a one-line **purpose** for the agent ("owns the REST API and DB
  schema"), stored on the membership record.
- On join, maiTerm injects an opener directive (reusing `buildOpener`) containing: the
  agent's role + purpose, the current roster, how topics work (set/propagate/complete, tag
  every message), and the agent's own status-note id (see §8).

---

## 7. View: stage + filmstrip

### 7.1 Layout

- A **two-panel stage** (single horizontal SplitPane, locked 50/50) shows two agents at full
  size: a **left** and a **right** slot.
- A **filmstrip** row across the bottom shows every other member, CSS-scaled down
  (`transform: scale(~0.25)`).
- **Swap:** click a filmstrip tile → promote to the **left** stage slot; shift+click →
  promote to the **right** slot. The demoted occupant returns to the filmstrip.

### 7.2 The fixed-cols invariant (critical)

Changing a terminal's column count mid-stream makes Claude Code re-render its whole
transcript, permanently duplicating scrollback blocks (documented project pitfall). The
mesh view must therefore **never change a terminal's cols when shifting layout.**

Rules:

- Every tile — both stage panels and every filmstrip thumbnail — renders at **one shared
  cols value** (the stage-panel width). The filmstrip is that same grid under a CSS scale
  transform. Promote = scale `0.25 → 1.0`. No reflow, no `resize_pty`, ever.
- `fitWithPadding` is **suppressed** in mesh view; cols are driven from a fixed mesh-panel
  width.
- The stage divider is **locked at 50/50** (no drag) so left and right are identical width —
  otherwise a left↔right swap would change cols. (Locked for v1; a coalesced/debounced
  draggable divider could come later.)
- Member join/leave reflows **only the filmstrip row**; stage cols stay constant regardless
  of member count (thumbnails just get smaller via scale).

### 7.3 Why this is cheap

The portal pattern already detaches terminals from their visual slot. Promote/demote is a
re-`attachToSlot` of a tabId to a different slot div; the terminal keeps running with no
teardown. Persistence across swaps is already solved.

### 7.4 Liveness / perf

Only the foreground tab gets a WebGL context, and a 0.25-scale terminal is unreadable
anyway. So filmstrip tiles are **throttled or frozen** — apply frames on activity-dot change
rather than at 60fps — and only the two staged panels stream live. The thumbnail's job is
"what color is the dot," not legibility.

---

## 8. Status board (workspace notes)

- **No tab notes** in mesh view. Instead, each agent maintains **one workspace note** keyed
  to its role.
- maiTerm pre-creates that note on join and hands the agent its `noteId` in the opener, so
  the agent updates one note rather than spawning many.
- The agent is instructed to keep it **concise**: what it completed, what needs a human
  decision, what it's blocked on.
- The notes panel becomes an **overlay side drawer** (floats over content) rather than
  pushing the stage to refit — this also protects the fixed-cols invariant, since opening
  the board must not resize any terminal.
- Phase-2 polish: an agent writing `NEEDS DECISION` raises a toast/badge so the human is
  pulled in instead of having to watch.

---

## 9. Visualization (the cheap magic)

- Emit a **conversation-edge event** inside `deliver()`: `{ sender, recipient, topic, turn,
  ts }`.
- **Mesh map:** nodes = agent tabs, edges = who-has-talked-to-whom, edge weight = traffic,
  an animated pulse along an edge = a message in flight. "Actively talking" = recipient
  state `active` and last edge < N seconds. Edges can be colored/filtered by topic.
- A visible **per-topic turn count** on the edge makes a runaway ping-pong *visible* so the
  human can intervene (one Esc on the staged panel interrupts).
- The filmstrip activity dots are the same signal in miniature: watch tiles light up, click
  one onto the stage to read it.

---

## 10. Loop control

No-broadcast + topic scoping + the "don't reply just to acknowledge" guard shrink runaway
risk substantially. A↔B ping-pong on one topic is still possible, but:

- The human is watching the board; runaways are *visible* via edge turn counts.
- Esc on a staged panel interrupts immediately (existing behavior).

For v1, visibility + manual interrupt is the control. Per-topic turn **budgets** are
deferred until observed need.

---

## 11. Data model changes

### Rust (`state/workspace.rs`)

```rust
// Workspace
bridge_all: bool,                  // this is a mesh workspace

// New: per-workspace topic registry (or a sibling store)
topics: Vec<MeshTopic>,

struct MeshTopic {
    id: String,
    label: String,
    owner_tab_id: String,
    state: TopicState,             // Open | Complete
    participants: Vec<String>,
}

// Tab (mesh membership) — extend agent_bridge or add:
mesh_role: Option<String>,         // descriptive role/name (required to join)
mesh_purpose: Option<String>,      // one-line human description
status_note_id: Option<String>,    // the agent's owned workspace note
```

The existing 1:1 `agent_bridge` field is superseded inside a mesh by roster membership
(presence in a `bridge_all` workspace), but the in-memory `delivery`/queue machinery is
unchanged.

### TypeScript

Mirror the structs in `tauri/types.ts`; extend `agentBridge.svelte.ts`:
`partnerTabId: string` → a roster (set of peer tab ids) when in a mesh, plus a topic map.

---

## 12. New MCP tools

| Tool | Purpose |
|---|---|
| `listBridgedPeers` | Roster: `{ role, tabId, cwd, purpose }[]` (generalizes `getBridgedAgent`). |
| `sendToBridgedAgent` (extended) | Add required `recipient` + required `topic`. No broadcast. |
| `listTopics` | Authoritative open topics `{ id, label, owner, participants }[]`. |
| `startTopic` | Create a topic (caller becomes owner). Optional — first send can also create. |
| `completeTopic` | Owner-only; marks complete and signals all participants. |

---

## 13. Decisions made

1. **Topic ownership** — the conversation's starting agent owns the topic; it sets the
   topic, the id propagates through all replies, and only the owner can complete it (which
   signals all participants to mark it done).
2. **Topic identity** — maiTerm-owned first-class objects with `listTopics`, not free-text
   strings.
3. **No broadcast** — every message is crafted for one recipient.
4. **Required handles** — a tab needs an explicit role name to join; the name is the
   address.
5. **Stage divider** — locked 50/50 for v1 to hold the equal-cols invariant.
6. **Notes board** — overlay side drawer, one workspace note per agent, no tab notes in
   mesh view.

---

## 14. Open questions

- Topic creation: explicit `startTopic` only, or allow implicit create-on-first-send? (Lean:
  allow both — a new label on send mints a topic.)
- What happens to an open topic when its owner tab closes? (Candidate: auto-complete with a
  "owner left" notice, or transfer ownership to the oldest participant.)
- Can the human be a first-class mesh node (address agents, set topics) from the
  composer/dock, or only observe + interrupt? (Lean: human can message any one agent and
  start/close topics, but is not auto-bridged into agent-to-agent threads.)
- Does the mesh map live as a corner overlay, a dedicated tab, or a toggle over the stage?

---

## 15. Phasing

1. **Graph on existing 1:1 bridges.** Emit edge events, draw the map/dots. Validates the
   who-talked-to-whom UX with zero routing risk. Highest magic-per-effort.
2. **N:M mesh core.** `bridge_all` workspace, roster + addressing, topic layer
   (create/propagate/complete), required role names, opener priming, status-note wiring.
3. **Mesh view.** Two-panel stage + filmstrip, fixed-cols + CSS-scale, locked divider,
   notes drawer, swap interaction.

Fastest end-to-end proof: a 3-agent roundtable in one mesh workspace, fixed-size tiles (no
width changes), roster + topic tools, status-note board, and the live edge overlay.
