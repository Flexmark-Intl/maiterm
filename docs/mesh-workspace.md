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
message; that topic id propagates through every reply in the thread. The owner can
**close/complete** the topic — and so can the **human**, from the cockpit (eng review D8;
this fixes the orphaned-topic case when the owner's tab is closed, decision 7). Completion
emits a `⟦TOPIC COMPLETE⟧` notice to every participant. That notice is a **maiTerm
control-plane signal, explicitly exempt from the no-broadcast rule** (§5): it is not an
agent crafting messages to N peers, it carries no conversational payload, and it is
delivered through the same per-recipient FIFO queue as everything else (so ordering/retry
semantics are identical). After a topic is complete, the send tool **rejects** further
sends on it (enforced at the tool boundary, not by trusting the model — Codex #9).

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
- **Addressing.** The send tool gains a `recipient` and a `topic`. Both are **optional on
  the shared `sendToBridgedAgent` schema** and **required-by-context at runtime** (mesh →
  required, 1:1 → recipient implicit). This keeps the existing 1:1 tool call valid and
  untouched (Codex #1; forced by eng review D2). `recipient` resolves against a **stable,
  maiTerm-minted handle**; the human-editable role name is the *display* label, not the
  routing key, so a rename/duplicate/casing change can't misroute (Codex #2).
  `listBridgedPeers` returns `{ handle, role, tabId, cwd, purpose }` per member.
- **Clarification is just a message.** If a role label is ambiguous to a peer, it asks the
  agent directly over the mesh — but routing itself never depends on the label, only the
  handle, so a send can't silently misroute on an ambiguous name.
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

### 7.2 Cols handling (best-effort, not a hard invariant)

Changing a terminal's column count mid-stream makes Claude Code re-render its transcript,
duplicating scrollback blocks (documented project pitfall). The mesh view should **avoid
gratuitous cols changes** when shuffling tiles — but per eng review D5 this is a best-effort
preference, **not** a hard guarantee we engineer against. A little duplicated history now and
then is an accepted tradeoff; we will not add a `resize_pty` guard that would break genuine
whole-window resizes.

Rules:

- Prefer **CSS scale** for tile↔stage transitions: tiles render near the stage-panel width
  and the filmstrip is that grid under a `transform: scale(~0.25)`. Promote = scale, not
  reflow. This keeps layout shuffles from firing width changes — but it's an optimization,
  not a tripwire.
- `fitWithPadding` is **not** run on layout shuffles in mesh view. Genuine window resizes
  still flow through the normal (coalesced) resize path untouched.
- The stage divider is **locked at 50/50** for v1 — a simplicity call (no drag handling, no
  per-swap width math), not a correctness requirement.
- Member join/leave reflows **only the filmstrip row**; the stage stays put.

### 7.3 Why this is cheap

The portal pattern already detaches terminals from their visual slot. Promote/demote is a
re-`attachToSlot` of a tabId to a different slot div; the terminal keeps running with no
teardown. Persistence across swaps is already solved.

### 7.4 Liveness / perf

Only the foreground tab gets a WebGL context, and a 0.25-scale terminal is unreadable
anyway. So filmstrip tiles are **throttled or frozen** — apply frames on activity-dot change
rather than at 60fps — and only the two staged panels stream live. The thumbnail's job is
"what color is the dot," not legibility.

**Open risks to spike before Phase 2 (Codex #10, #11):** (a) the app currently assumes a
single foreground terminal owns the live renderer; **two** simultaneously-live stage panels
may need renderer-ownership changes deeper than view-only CSS. (b) N full terminal grids
scaled down (each with its own buffer/scrollback/DOM) may cost more than "it's just a CSS
transform" implies. Both are validated by a short spike that gates the view phase — if two
live renderers or a heavy filmstrip prove costly, fall back to snapshot-frozen filmstrip
tiles (lose the live-dot signal) or a single live stage panel.

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
risk, but they do not bound it — A↔B ping-pong on one topic can still burn tokens and thrash
the recipient PTYs while nobody is watching. So loop control is **v1, not deferred** (eng
review D4 + Codex #4), in three layers:

1. **Soft per-topic turn cap (primary).** At `N` turns on a topic, delivery pauses and the
   cockpit surfaces "topic paused at N turns — resume / complete?". The human resumes or
   completes. Reuses the per-topic turn counter already tracked for the map (no new state).
2. **Hard TTL ceiling (backstop).** A topic that reaches a hard turn ceiling `M` (`M ≫ N`)
   or has no activity for a TTL is force-paused regardless — a backstop for the
   away-from-keyboard case so an unwatched runaway can't run unbounded.
3. **Visibility + interrupt (always).** Edge turn counts on the map make runaways visible;
   Esc on a staged panel interrupts immediately (existing behavior).

`N` and `M` are preferences with sane defaults, not hardcoded.

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
7. **Owner tab closes** — do nothing; the topic stays `open`. A tab may reopen/resume, so
   the mesh must not tear down or reassign its topics on close.
8. **Human is a director, not a mesh peer** — the human tells an *agent* what needs done;
   that agent decides who to talk to and how to name the topic. The human steers by
   interjecting on any agent's panel mid-flight (type into it / Esc), course-correcting as
   the work proceeds. The human does not address peers or set topics through a mesh-level
   control; they work *through* agents.
9. **The drawer is the cockpit** — the overlay side drawer hosts both the status board AND
   the mesh map (and any further at-a-glance details we add). One place the human glances at
   to see who's talking, on what topic, and what each agent has done / needs.

---

## 14. Open questions

- **Topic creation mechanics** (for eng review): explicit `startTopic(label)` first, or
  implicit create-on-first-send where a new `topic` label mints a topic owned by the sender?
  What is the cleanest registry/ownership/dedup implementation? (Decision deferred to
  `/plan-eng-review`.)

---

## 15. Phasing

Reordered per eng review D7 (Codex #14/#15): build the mesh CORE first to retire the real
system risk; the graph rides along nearly free because edge events live in `deliver()`. The
elaborate view is last (its perf/renderer questions, §7.4, are the most uncertain work).

1. **Mesh core (headless) + graph.** Extract the shared delivery core out of `agentBridge`
   (regression-tested against the 1:1 FIFO behavior); `bridge_all` workspace, derived
   roster + addressing (stable handle behind the display role-name), topic layer
   (create-on-first-send with normalized-label dedup, propagate, complete, reject-on-
   completed), soft cap + hard TTL, opener priming, status-note wiring. Emit conversation-
   edge events in `deliver()` and render the map/dots in the cockpit drawer. Agents run in
   normal tabs/splits for this phase — no custom layout yet. This slice is genuinely usable
   on its own (N:M agents talking, human reads the board) and proves every hard part.
2. **Mesh view.** Two-panel stage + scaled filmstrip, CSS-scale promote/demote, locked
   50/50 divider, swap interaction. Gated on a quick spike validating §7.4 (two live stage
   renderers + N scaled filmstrip grids) before committing to the layout.

Fastest end-to-end proof: a 3-agent roundtable in one mesh workspace, roster + topic tools,
status-note board, and the live edge overlay — all in Phase 1, agents in normal splits.

---

## 16. Engineering Review Outcome (/plan-eng-review)

### 16.1 Decisions

| # | Decision | Choice |
|---|---|---|
| D1 | Build sequencing | Phase it (refined by D7). |
| D2 | Mesh routing | **Derived roster + shared delivery core.** Roster = named tabs in the `bridge_all` workspace (not persisted per-tab). Extract delivery map + deliver/flush/deliverable/cooldown + envelope + priming out of `agentBridge` into a shared `agentDelivery` module both 1:1 and mesh consume. 1:1 persistence untouched. |
| D3 | Topic storage | **Model on `WorkspaceNote`.** Topic = first-class object persisted as `Vec<MeshTopic>` on `Workspace`, mirroring the `workspace_notes` CRUD/MCP-tool pattern. |
| D4 | Loop control | **Soft per-topic turn cap** (pause + resume) as primary, **hard TTL/ceiling backstop** for the unwatched case (Codex #4). v1, not deferred. See §10. |
| D5 | Cols handling | **Best-effort CSS-scale, no hard guard.** Whole-window resizes flow normally; occasional duplicated history is an accepted tradeoff. See §7.2. |
| D6 | Outside voice | Ran Codex (gpt-5.5). 15 findings — folded below. |
| D7 | Phasing | **Core-first, graph rides along, view last** (Codex #14/#15). See §15. |
| D8 | Human controls | **Human can complete any topic** from the cockpit (one minimal mesh control) — resolves orphaned topics (Codex #3). See §4.1. |

### 16.2 Codex folds (resolved, baked into the sections above)

- **#1 tool compat** → `recipient`/`topic` optional on the shared tool, required-by-context at runtime (§5).
- **#2 fragile names** → route by stable minted handle; role name is display only (§5).
- **#5 no-broadcast vs completion** → completion is a control-plane signal, explicitly exempt, same FIFO delivery (§4.1).
- **#7 create race/dup** → create-on-first-send with **normalized-label dedup** (reuse an open topic whose normalized label matches) (§15 P1).
- **#9 model compliance** → **reject sends on a completed topic** at the tool boundary (§4.1).
- **#4 loop control** → hard TTL ceiling added above the soft cap (§10).

### 16.3 Deferred with eyes open (NOT in scope for v1)

| Item | Rationale | Tracked |
|---|---|---|
| Topic-aware delivery prioritization (Codex #8: one noisy topic head-of-line-blocks urgent msgs to the same agent via per-recipient FIFO) | Acceptable for a few agents with the soft cap bounding any one topic; revisit if starvation observed. | TODO |
| Structured status record vs freeform notes (Codex #12) | v1 uses workspace notes with a `NEEDS DECISION:` line convention the toast scans; a structured `MeshStatus` record is better for badges/filtering but more build. | TODO |
| Two-live-renderer + filmstrip DOM cost (Codex #10/#11) | Gated by a spike before the view phase (§7.4); fallbacks defined. | §7.4 / Phase 2 gate |
| Ownership delegation (Codex #6) | D8's human-complete covers the orphan case; auto-delegation deferred unless fan-out makes it painful. | TODO |
| Draggable stage divider | Locked 50/50 for v1 simplicity (§7.2). | Future |
| Multi-instance / server-authoritative topics | Single local app instance; frontend-mutated registry is sufficient. | Future |

### 16.4 What already exists (reuse, not rebuild)

See §2. Net: the delivery queue, envelopes, persistence/self-heal, priming, portal rendering,
workspace-notes + tools, `SplitNode` 50/50 split, and `forkSessionIntoSplit` are reused. The
delivery-core **extraction** (D2) is the one change to existing code, and it is a refactor of
the working 1:1 path — covered by a mandatory regression test (16.6).

### 16.5 Failure modes

| Codepath | Failure | Test? | Error handling | Visible? |
|---|---|---|---|---|
| `deliver()` edge emit | edge fires on `queued` → phantom map edge | yes (emit on `delivered` only) | n/a | n/a |
| delivery-core extraction | 1:1 FIFO order regresses | **REGRESSION-CRITICAL** | n/a | would be silent → test mandatory |
| `sendToBridgedAgent` mesh | unknown recipient handle | yes | **clear tool error + roster** (no silent drop) | yes |
| `MeshTopic` persistence | serde `skip_serializing_if` null/undefined drift (documented pitfall) | yes (round-trip) | field-by-field `?? null` compare | n/a |
| soft cap | never resumes → topic wedged | yes (resume path) | hard TTL backstop | yes (cockpit) |
| completed topic | agent keeps sending | yes | tool rejects send | yes (tool error) |

**Critical gap guard:** unknown-recipient must return a clear error with the roster — never a
silent drop. This is the one path that would otherwise fail silently.

### 16.6 Test plan (coverage contract)

Built alongside the code, not deferred. **Mandatory regression (IRON RULE):** the
`agentDelivery` extraction ships with a test proving the 1:1 bridge still delivers strictly
FIFO (locks in `e3d4eb8`). Phase-1 (Rust unit): topic create/dedup/complete/owner-only +
human-complete, reject-on-completed, roster derivation (join/close/rename/non-agent-exclude),
soft-cap pause/resume, serde round-trip. Phase-1 (TS): routing (missing/unknown recipient →
error; self-send guard; routed to recipient queue), edge-event-on-delivered-only. Phase-2
(component): attachToSlot swap persists the terminal; layout shuffle does not refit; drawer
overlay does not refit.

### 16.7 Implementation Tasks

Synthesized from findings. P1 blocks the phase; P2 same-phase; P3 follow-up.

- [x] **T1 (P1) — DONE (`170229c`)** — delivery core — extracted `delivery`/queue/cooldown/
  drain into a shared, dependency-injected `agentDelivery.ts`; `agentBridge` consumes it.
  - Verify: **regression suite** `agentDelivery.test.ts` (6) — FIFO + e3d4eb8 no-queue-jump locked.
- [x] **T2 (P1) — DONE (`0989436`)** — topics — `MeshTopic` + `MeshTopicState` on `Workspace`
  (modeled on `workspace_notes`); `normalize_label` dedup key; `bridge_all` + `mesh_topics`
  (both `serde(default)`); TS mirrors.
  - Verify: Rust `mesh_topic_tests` (4) — dedup, owner round-trip, defaults, pre-mesh back-compat.
- [x] **T3 (P1) — DONE** — routing — `set_workspace_bridge_all` + `set_workspace_mesh_topics`
  commands (+ TS wrappers); pure `meshRouting.ts` (stable-handle resolution, topic registry,
  create-on-first-send + dedup, owner/human complete, reject-on-completed) and pure
  `meshSend.ts` (envelope → deliver → commit; edge-on-delivered-only); `agentMesh.svelte.ts`
  live store (derived roster, member readiness, persistence, edge ring); `protocol.rs`
  schemas (extended `sendToBridgedAgent` + `listBridgedPeers`/`listTopics`/`startTopic`/
  `completeTopic`); `claudeCode.svelte.ts` mesh-vs-1:1 dispatch; lifecycle wired in
  `+layout`/`+page`/`workspaces`.
  - Verify: `meshRouting.test.ts` (18) + `meshSend.test.ts` (8) — unknown/ambiguous recipient →
    error (no silent drop), self-send guard, dedup, reject-on-completed, edge-on-delivered-only,
    routed-to-recipient-queue (real-controller FIFO integration).
- [x] **T4 (P1) — DONE** — loop control — pure `meshLoopControl.ts`: soft cap N (liftable by
  human resume), hard ceiling M (complete-only backstop), TTL (time backstop). Gated into
  `meshSend.ts` (a paused send neither injects nor advances the turn). Prefs `mesh_soft_cap`
  (12), `mesh_hard_cap` (40), `mesh_topic_ttl_minutes` (30) on Rust + TS + the prefs store.
  Cockpit APIs on `agentMesh`: `resumeTopic`, `topicPauseInfo`, `pausedTopics`; `listTopics`
  surfaces `paused`/`pauseReason`. (Prefs UI lands with the cockpit in T6.)
  - Verify: `meshLoopControl.test.ts` (8) — pause-at-N, resume-lifts, hard-can't-resume,
    TTL pause + re-base, precedence (hard > ttl > soft); `meshSend.test.ts` paused-send case.
- [x] **T5 (P1) — DONE** — priming + status — on join, `tryPrime` (idempotent) injects a mesh
  opener (role, purpose, roster, topic rules, the agent's status-note id) and pre-creates ONE
  workspace status note per role, reused-by-marker across re-prime/restart (a fresh agent is
  primed, a returning one isn't re-onboarded). Pure `meshStatus.ts` (template + marker +
  `parseNeedsDecision`); a `NEEDS DECISION:` block in any status note raises a deduped toast
  deep-linked to the owning tab, scanned from the workspace-note write path. Priming fires on
  `agent-init-session`, `agent-hook-stop` (catches a late-named agent), and mesh-enable.
  - Verify: `meshStatus.test.ts` (8) — marker/template, placeholder→empty, single/multi-item
    extraction, stop-at-next-heading, case-insensitive.
- [x] **T6 (P1) — DONE** — cockpit drawer — `MeshCockpit.svelte`, a right-side overlay drawer
  (⌘⇧M, or the sidebar MESH badge) over the active workspace: an enable-mesh CTA for a normal
  workspace; for a mesh — a paused-topic banner (resume/complete), the conversation graph
  (pure `meshGraph.ts` circular layout, owner→participant topic stars weighted by turns,
  per-topic color, live pulse on recent deliveries, active-agent halo), the topics list
  (turns + human Complete/Resume, D8), and the status board (per-agent parsed note with
  NEEDS DECISION / Blocked / Done, editable purpose, deep-link to the tab). Prefs UI for
  N/M/TTL added to the AI Agents settings section.
  - Surfaced by: §9, D8. Files: `MeshCockpit.svelte`, `meshGraph.ts`, `+layout.svelte`
    (shortcut + render + open-mesh-cockpit event), `WorkspaceSidebar.svelte` (badge),
    `preferences/+page.svelte`.
  - Verify: `meshGraph.test.ts` (7) — circle layout, active flag, owner-star edges wired to
    node positions, completed/absent exclusion, recent-pulse window, paused flag.
- [ ] **T7 (P2, spike first)** — mesh view — stage + filmstrip, CSS-scale swap, locked 50/50.
  - Surfaced by: §7, D7. **Gated on the §7.4 renderer/perf spike.**

### 16.8 Parallelization

| Lane | Tasks | Notes |
|---|---|---|
| A | T1 → (T3, T4) | Shared `agentDelivery` core; sequential within lane. |
| B | T2 | Rust topic model + commands; independent of A until T3 wires them. |
| C | T5, T6 | Priming + cockpit; depend on T2/T3 contracts but UI-side, parallel to A/B once tool shapes are fixed. |

Launch A and B in parallel worktrees. T3 joins them (touches both the delivery core and the
topic tools) → run after T1 and T2 land. C follows once the tool/contract shapes from T2/T3
are fixed. T7 (view) is a separate later phase after the spike. **Conflict flag:** T1 and T3
both touch the delivery/store layer — keep them in the same lane (sequential), not parallel.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 0 | — | not run |
| Codex Review | `/codex review` | Independent 2nd opinion | 1 | issues_found | 15 findings; 9 folded, 2 → user tensions (D7/D8), 4 deferred |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | clean | 5 decisions (D1-D5) + 2 tension resolutions (D7/D8); 1 critical gap (unknown-recipient) has error handling + test |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | — | not run (view deferred to Phase 2) |
| DX Review | `/plan-devex-review` | Developer experience gaps | 0 | — | n/a |

- **CODEX:** 15-point challenge; phasing (#14/#15) and human-controls (#3/#6/#13) escalated to user as D7/D8, both resolved. Tool-compat, fragile-names, completion-broadcast, create-race, model-compliance, hard-TTL all folded into the doc.
- **CROSS-MODEL:** two tensions surfaced; both resolved by user decision (D7 core-first, D8 human-complete). No unresolved tensions.
- **UNRESOLVED:** none.
- **VERDICT:** ENG CLEARED — design is reviewed and ready to implement Phase 1. Design Review recommended before Phase 2 (the view). No code written in this review.
