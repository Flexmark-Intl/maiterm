# Agent requests the human approves

2026-10-10. Darryl: "mcp needs ability to manage mesh/mesh groups, but explicitly only permitted
with direct user auth; likewise ability to run update & restart with explicit auth from human",
and "for organization and read operations it doesn't require the prompt".

## 1. What agents can do

| Tool | Acts at once? |
|---|---|
| `getMeshLayout` | yes: read-only |
| `moveTab` | yes, unless it changes the tab's mesh |
| `moveWorkspace` | yes, unless the workspace is a mesh carrying a link |
| `setMesh` | never: a request |
| `linkMeshes` | never: a request |
| `unlinkMesh` | never: a request |
| `restartMaiterm` (`update: true` installs the update first) | never: a request |
| `getRequestStatus` | yes: read-only, the caller's own requests only |

**The line is "who can message whom".** The mesh is the one place the human decides which agents
may talk (docs/mesh-workspace.md §17; the maiterm-tab mod refuses Claude's own cross-session
messages for the same reason). Any move that changes a tab's mesh is a mesh change:
- **A tab moved within a window:** it needs approval when the source and destination resolve to
  different meshes (`meshKey`: none, the workspace itself, or its group).
- **A tab moved across windows:** it needs approval when either side is a mesh, because a mesh
  never spans windows.
- **A workspace moved:** it needs approval when it is a mesh with a `mesh_group`. The move breaks
  the link in this window, and the workspace rejoins any workspace holding that group in the
  target window.

## 2. Where the approval lives

`src-tauri/src/agent_requests.rs` keeps the requests: in memory, process-wide, and none survive a
restart. A request moves through these states:

| Status | How it gets there |
|---|---|
| `pending` | filed |
| `approved` | the human approves |
| `done` / `failed` | the window carrying it out reports back |
| `denied` | the human denies |
| `expired` | 30 min with no answer |

An approval nobody reports back on within 5 min becomes `failed`.

**Only `decide` moves a request out of pending.** Two things call it:
- the Tauri command behind the card's button (`decide_agent_request`);
- in protocol 0.23, the phone's authenticated endpoint.

No MCP tool reaches either. Two things were considered and rejected as the approval:
- **The agent's AskUserQuestion.** Its answer is whatever the agent says it was.
- **Claude's tool-permission dialog.** An allowlist (`mcp__maiterm__*`) or auto mode passes it
  without the human seeing it.

**The card's words are maiTerm's.** The handler builds the summary and the effects from maiTerm's
own state when the request is filed. The agent supplies only `reason`, and the card shows it as a
quote. The window carries out the request from the arguments stored in Rust, and first re-checks
that the world still matches the card:
- a tab that has moved since, fails;
- for an update, a different version on offer than the one approved, fails.

**A mesh request stores what its card named.** It records the workspaces of each mesh involved
and the agent tabs in them. The executor refuses ("the meshes changed… ask again") if those
differ at approval. Moves that don't touch a mesh are ungated, so without this check an agent
could fill a plain workspace with agents between the card and the click. The approval would then
link agents the card never mentioned (review of ffb76a7f). `mesh_enable` is the one exception:
its card says agents added later join too.

**The phone approves with a paired device's token only** (`authorize_human`). The dev bearer
token is in plain text in the maiLink data dir, and the API listens on every interface, so any
local agent could otherwise `curl` its own approval (review of c11db1f3). The watch-script
approve and reject routes got the same gate. **Still open:** the dev token is also accepted on
`/respond` (permission and question answers) and on task answers.

**Identity.** Every tool that changes something is on `PEER_ADDRESSING_TOOLS`, so it is refused on
an inferred identity. The ones that file requests, and `getRequestStatus`, are also on
`OWN_TAB_ONLY_TOOLS`, because the card names the caller as the agent asking.

**Limits.**
- A tab may hold at most 5 pending requests.
- Filing an identical pending request returns the first one instead of adding a second card.

## 3. The path

1. The tool routes to the window that owns its target (`target_owner_window` in server.rs: by
   `tab_id` or `workspace_id`). Only that window holds the target in its stores.
2. That window's handler (`agentRequests.svelte.ts`) acts at once, or calls `file_agent_request`.
   Filing raises a request notice that opens the Loom's Decisions when clicked.
3. The human answers on `AgentRequestCard` (the Decisions queue). The card has the same click
   guards as `ScriptApprovalCard`:
   - nothing is decided in a card's first 1.5 s after it appears or its list moves;
   - the second click of a double-click is ignored.
4. Rust emits `agent-request-approved` to the filing window, which carries it out and calls
   `settle_agent_request`.
5. `agent-request-settled` reaches every window. The one holding the requesting tab's terminal
   tells its agent through `overlordStore.tellTab`, the board-notice path: idle, or typed into a
   busy Claude tab's empty box, never over a draft.
   - A restart is not reported this way: the agent is restarted instead.

## 4. The phone

maiLink protocol 0.23, §4.4: `GET /requests`, `POST /requests/{id}/approve|deny` (`decide` with
`by: "phone"`), the WS `requests` frame, `Chat.requestsWaiting`, and the doorbell kind `request`,
pushed into the doorbell's ring queue when a request is filed. An approval reply waits up to 15 s
for the outcome, except for a restart.

## 5. Not done yet

- **Codex tabs** hear the outcome only when idle (the busy-tab path is Claude-only), and otherwise
  through `getRequestStatus`.
