# Claude Code IDE Integration

maiTerm exposes an MCP server that Claude Code CLI discovers and connects to, providing IDE-like capabilities.

## Architecture

```
Claude Code CLI ←→ WebSocket/SSE ←→ axum server (Rust) ←→ Tauri events ←→ Frontend (Svelte)
```

**Backend** (`src-tauri/src/claude_code/`):
- `server.rs` — axum router with WebSocket (`/`) and SSE (`/sse` + `/message`) endpoints. Random port (10000–65535), 32-char auth token.
- `protocol.rs` — JSON-RPC request/response types, `tool_list_response()` (50 tools), `initialize_response()`
- `lockfile.rs` — writes `~/.claude/ide/{port}.lock` for discovery, registers `mcpServers.maiterm` (or `maiterm-dev`) in `~/.claude.json` (stripping the legacy `aiterm`/`aiterm-dev` key on write — rebrand migration), registers hooks in `~/.claude/settings.json`

**Frontend** (`src/lib/stores/claudeCode.svelte.ts`):
- Listens for `claude-code-tool` Tauri events
- Dispatches to tool handlers (getOpenEditors, openFile, openDiff, etc.)
- Responds via `claude_code_respond` Tauri command

**Enabled by**: `preferences.claude_code_ide` (default true). Server startup is two-phase: `prepare_server()` runs synchronously inside Tauri's `setup()` (binds the TCP port, writes `~/.claude.json`, hooks, skill), then `serve_server()` runs as a background tokio task that adopts the pre-bound listener and runs `axum::serve`. The sync prep is load-bearing — frontend doesn't load (and no PTY can auto-resume `claude --resume …`) until `~/.claude.json` has the current port.

## Tools Exposed

| Tool | Description |
|------|-------------|
| initSession | **REQUIRED first call.** Registers tab ID + session ID → enables auto-inject of tabId on all subsequent calls. Safe to run in parallel with non-maiterm opening tool calls (file reads, grep) to save a round-trip — but never batched with other maiterm calls, which would race the registration |
| getOpenEditors | List open editor tabs (path, language, dirty state) |
| getWorkspaceFolders | Workspace root paths |
| getDiagnostics | App version, tab/PTY counts, WebGL status, FPS, memory/CPU, performance metrics |
| checkDocumentDirty | Check if file has unsaved changes |
| saveDocument | Save file to disk |
| getCurrentSelection | Active editor selection + cursor |
| getLatestSelection | Most recent selection in any tab |
| openFile | Open file in editor tab (with optional line/text selection) |
| openDiff | Show side-by-side diff for review (blocking) |
| showDiff | Open read-only diff tab comparing file to a git ref (default HEAD) |
| closeAllDiffTabs | Close all pending diff tabs |
| listWindows | List all maiTerm windows with IDs, labels, and workspace summaries |
| listWorkspaces | List all workspaces with panes, tabs, archived tab count (IDs, display names, types, active state, notes, Claude state) |
| switchTab | Navigate to a tab by ID (auto-resolves workspace/pane) |
| getTabNotes | Read notes for a tab (optional tabId, defaults to active) |
| setTabNotes | Write/clear notes for a tab |
| editTabNotes | Precision edit: find old_string in notes, replace with new_string (must match uniquely) |
| listWorkspaceNotes | List workspace-level notes (IDs, previews, timestamps) |
| readWorkspaceNote | Read full content of a workspace note |
| writeWorkspaceNote | Create or update a workspace note |
| deleteWorkspaceNote | Delete a workspace note |
| moveNote | Move note between tab and workspace (with conflict detection) |
| getTabContext | Get recent terminal output/editor content for tab discovery |
| openNotesPanel | Open/close/toggle the notes panel for the active tab |
| setNotesScope | Switch notes panel between 'tab' and 'workspace' views |
| getActiveTab | Get the currently active workspace, pane, and tab info |
| setTriggerVariable | Set/clear a trigger variable (e.g. claudeSessionId) for a tab |
| getTriggerVariables | Read all trigger variables for a tab |
| setAutoResume | Enable/disable auto-resume with optional command/cwd/ssh overrides |
| getAutoResume | Get current auto-resume configuration for a tab |
| findNotes | Search all tabs and workspaces for notes, returns previews |
| sendNotification | Send in-app toast notification (title, body, type) |
| readLogs | Read recent maiTerm log entries (filterable by level, search string) |
| getPreferences | Return current preferences with metadata (filterable by query) |
| setPreference | Update a single preference by key |
| createBackup | Create gzip-compressed backup of entire maiTerm state |
| getClaudeSessions | All active Claude sessions across tabs (state, tool, model, cwd) — multi-agent coordination |
| listArchivedTabs | List archived (suspended) tabs with names, dates, restore context |
| restoreArchivedTab | Restore an archived tab back into the active workspace |
| sendToBridgedAgent | Send a message to a peer agent. 1:1 bridge: omit recipient/topic. Mesh Workspace: recipient (role/handle) + topic required. Async — reply arrives as a new prompt turn |
| getBridgedAgent | Report whether this tab is bridged and, if so, the partner's label/cwd (in a mesh, returns the roster) |
| listBridgedPeers | Mesh only: roster of reachable peers — handle (tabId), role, cwd, purpose, live |
| listTopics | Mesh only: conversation topics — id, label, state, owner, participants, turn count |
| startTopic | Mesh only: start/reuse a topic (caller becomes owner); returns the topic id |
| completeTopic | Mesh only: owner marks a topic complete; signals participants, rejects further sends |
| bindCommsThread | Comms (/maiterm resolve): bind this tab to a Mattermost thread by permalink; returns the thread as a [REPORT]-tagged transcript, the bot's `bot_username`, and records `Tab.comms_binding`. Backend-only |
| readCommsThread | Comms: re-fetch the full bound thread on demand (only @mentions of the bot are auto-injected; the rest is read-on-demand). Backend-only |
| postCommsReply | Comms: post Mattermost markdown to the bound thread; `resolve: true` clears the binding after posting. Backend-only |
| unbindCommsThread | Comms: clear the tab's thread binding without posting (idempotent). Backend-only |

## Comms Integration (/maiterm resolve)

Binds a tab to an external chat thread (Mattermost; `provider` field is the Slack seam) so an
agent can pull a bug-report thread as a work item and post a resolution back. Module:
`src-tauri/src/comms/` (thin bot-token REST client + permalink parsing + the reply watcher).

- **Config**: `Preferences.comms_provider` / `comms_server_url` / `comms_bot_token` (single
  account per install; set in Preferences → Integrations, test via the `comms_test_connection`
  command). **Invariant: `comms_bot_token` must never be added to `preference_meta()`** — that
  omission is what keeps the token unreadable via getPreferences/setPreference.
- **Operator instructions**: `Preferences.comms_instructions` — free-text operator guidance for
  how the agent communicates on threads. Delivered as `operator_instructions` in the
  bindCommsThread / readCommsThread results (governs communication only; the authority/safety
  rules are not overridable by it). Also absent from `preference_meta()` so no chat message can
  rewrite the agent's harness.
- **Bindings**: `Tab.comms_bindings: Vec<CommsBinding>` (provider, server_url snapshot,
  channel_id, root_id, permalink, last_seen_create_at cursor, bound_at) — a tab can work several
  threads at once (the skill has the agent fan out subagents and pass `root_id` explicitly on
  every comms tool call when more than one is bound; the tools error on ambiguity). Legacy
  single `comms_binding` is drained into the list by a load migration in persistence.rs.
  Persisted — survives restart, dies with the tab; never cloned by tab/workspace duplication
  (one thread = one tab, or the watcher would double-inject). A thread bound to one tab refuses
  to bind to another.
- **Chat monitoring (summons)**: `Tab.comms_monitor` (`CommsMonitor{channels}`) — operator
  right-clicks a tab → "Enable chat monitoring…" → `CommsMonitorModal` (channels the bot is a
  member of, via `comms_list_bot_channels`; persisted via `set_tab_comms_monitor`, new channels'
  cursors start at now). The watcher's summon phase polls each monitored channel
  (`channel_posts_since`); an @bot mention in an UNBOUND thread by a user on
  `comms_pickup_users` (or an authorized user) auto-binds that thread to the monitor tab and
  injects a `[Mattermost pickup …]` message with the full transcript. Post-by-post cursor walk
  queues naturally: at capacity (`MAX_TAB_BINDINGS` = 3), busy, or offline, the cursor HOLDS at
  the summon (one-time in-thread "at capacity" reply + operator `comms-summon` notification) and
  retries when the tab frees; unauthorized mentions notify the operator only, nothing in-thread.
  Pickup users are summon-only — their messages stay [support]-tier during the work.
  Summon candidates require `create_at` past the cursor: Mattermost's `?since=` is
  **update_at-based**, so an EDIT of an old post is re-served — without the guard, editing the
  root of a just-resolved (unbound) thread re-picked the whole thread up as fresh work.
- **Image attachments (both directions)**: incoming — `Post.file_ids`/`metadata.files` are
  deserialized; `comms::stage_attachments` downloads image files (png/jpg/gif/webp, ≤10 MB,
  ≤8/call) and stages them where the tab's agent can Read them (`staging_target_for_tab`:
  local temp dir, or remote /tmp over the bridge tunnel via `mailink::push_bytes_remote` for
  SSH tabs; SSH-without-tunnel degrades to a "cannot be staged" note). Staged paths appear as
  `[attached image … staged at <path> — view it with the Read tool]` lines in bind/read
  transcripts, watcher injections, and summon pickups. Outgoing — `postCommsReply` takes
  `attachments: [paths]` (max 5, ≤20 MB): local tabs read the files directly; SSH tabs fetch
  the agent's remote paths back over the tunnel (`mailink::fetch_bytes_remote`), then
  `upload_file` (multipart POST /api/v4/files) → `create_post` with `file_ids`.
- **Watcher** (`comms::watcher_loop`, spawned unconditionally in `lib.rs` setup): every 5s scans
  tabs for bindings, fetches each bound thread, and injects **only posts that @mention the bot's
  own username** (`mentions_username`, cursor-newer, not-the-bot, non-empty) into the tab's PTY
  via `mailink::inject_text` as one bracketed paste per tick. Ambient thread chatter is never
  pushed — it's read-on-demand via `readCommsThread`; the cursor advances past it so it isn't
  re-scanned. Holds (cursor unadvanced) while the tab has no live agent session or PTY — and
  rings the operator: an undeliverable @bot reply emits `comms-reply-pending` (handled in
  `+layout.svelte` → `notificationDispatch`, deep-links to the tab), deduped per newest post so
  a held burst notifies once, re-armed after a successful delivery. Resuming the session
  delivers the held replies on the next tick. Exponential per-binding backoff on errors; auth
  failures logged once per config fingerprint.
- **Authority tiers**: each injected message is stamped `[AUTHORIZED]` or `[support]`. Authorized
  = author's username is in `Preferences.comms_authorized_users` (matched case-insensitively);
  those messages carry full operator authority. Everyone else is scoped (investigate + reply
  only; destructive/scope-expanding actions need operator confirmation — enforced by SKILL.md
  framing, not a hard sandbox, since the agent runs in a PTY maiTerm can't intercept).
  **`comms_authorized_users` is deliberately absent from `preference_meta()`** so no chat message
  can edit who is trusted — only the human via Preferences → Integrations.
- **Operator kill switch**: a bound tab shows a green `@` indicator in `TerminalTabs.svelte`; its
  context menu gains "End thread binding" → `clear_tab_comms_binding` command, which clears
  `Tab.comms_binding` directly (no agent involvement, posts nothing). The watcher re-reads
  bindings each tick, so forwarding stops within ~5s. This is the human's override when the agent
  is stuck/misbehaving — severing never depends on the agent cooperating.
- **Async dispatch**: the comms tools made `handle_backend_tool` async (awaited at its single
  call site in `process_message`). New arms must never hold a lock guard across an await.
- **Skill**: the `resolve` section of `resources/maiterm-skill/SKILL.md` is the agent-facing
  orchestration (silent-while-working, one `**@Support:**`/`**@Dev:**`-addressed question when
  blocked, two-part resolution post: plain-language for support staff, `---`, technical bullets
  for devs). Posting the resolution does NOT unbind — the thread stays bound until a human
  confirms it's resolved; only then does the agent close it (`postCommsReply` with `resolve:true`,
  which posts-and-clears). A still-broken reply keeps the binding live so work continues.

## Agent Bridge (agent-to-agent bridge)

Lets two running Claude agents in different panes talk to each other. The human bridges
the active tab to another running Claude session via the **Agent Bridge picker**
(`Cmd+Shift+L`, or terminal context menu → "Connect to Agent…"). The agents then converse
asynchronously via the `sendToBridgedAgent` tool; each message is injected as a real
terminal turn in the recipient's pane, so the human watches the whole exchange and can
interrupt with Esc.

**Two bridge modes (picker toggle):**
- **Fork into new pane** (default) — `establishBridge()` forks the target session
  (`claude --resume <id> --fork-session`) into a split pane beside the caller: an
  isolated peer with the target's full context that doesn't disturb the original.
- **Connect existing tab** — `bridgeExistingTab()` bridges two already-running Claude tabs
  directly, no fork/new pane (for when the split already exists, e.g. a failed
  auto-reconnect). Idempotent: re-selecting the caller's own partner *repairs* a broken
  bridge in place; it refuses to hijack a tab bridged to a third agent.

**Human-guided opener:** the picker has an optional textarea where the human describes
the peer (what it's expert on / how to use it). The opener fed to the calling agent does
NOT fire questions immediately — it tells the agent to check in with the human first
(summarize what the peer offers, propose a few things it could ask) and wait for
direction before using `sendToBridgedAgent`. The description is in-memory (one-time, not
persisted).

**Key files:**
- `src/lib/stores/agentBridge.svelte.ts` — bridge registry (keyed by tab_id, symmetric),
  `establishBridge()` (fork + handshake), `bridgeExistingTab()` (no-fork bridge/repair),
  `primeFork()` (auto-init directive), `rehydrate()` (rebuild from persisted state),
  delivery gating, identity envelopes
- `src/lib/stores/workspaces.svelte.ts` → `forkSessionIntoSplit()` — splits the caller's
  pane and boots the forked partner (reuses the clone/auto-resume spawn path with
  `setSplitContext({ fireAutoResume: true })`)
- `src/lib/components/AgentBridgePicker.svelte` — session picker modal (mode toggle + purpose textarea)
- `claudeCode.svelte.ts` → `handleSendToBridgedAgent` / `handleGetBridgedAgent` dispatch
- Persistence: `Tab.agent_bridge` (Rust `AgentBridge` struct) via the `set_tab_agent_bridge`
  command — durable pairing written both sides

**Design decisions (v1):** async-only (no blocking RPC); fork-only (the fork *is* the
target, isolated); loop control = framing + human Esc (no circuit breaker); identity is
stamped by maiTerm from the registry (tamper-proof — recipient can't mistake a peer for
the human); bridge keyed by tab_id (survives the fork's new session id).

**Handshake (tight, routing-proof):** a forked session resumes the target's transcript,
so it inherits the target's `initSession` and won't re-bind its new MCP connection. After
the fork's Claude boots, `primeFork()` injects a directive forcing it to re-`initSession`
as its OWN tab; that tab's real `claude-init-session` event is the handshake trigger
(proves it's up, on this instance, tool-capable) — not a flaky `SessionStart` hook.

**Delivery readiness model:** per-tab `ready` (caller immediate; fork once its
`initSession` lands), `busy` (set on inject, cleared on that tab's `Stop`),
`hasCompletedTurn` (once true, `claudeState` active/idle is trusted — and a live session
is required, so a dormant/resuming partner queues instead of injecting into a shell).
Messages to a busy/dormant tab queue and flush on its next `Stop`/re-init.

**Persistence & resume (hardening):** the pairing is persisted on both tabs (`Tab.agent_bridge`),
so a bridge survives an app restart. On load, `rehydrate()` rebuilds the in-memory registry
for any pair where both tabs still exist and reciprocally reference each other (orphans
cleared). `session-end` only **suspends** the in-memory bridge (keeps the durable pairing) —
the agent may auto-resume and re-bind; only an explicit disconnect or a closed tab tears it
down. Because `claude --resume` can mint a new session id, the recorded `partner_session_id`
is refreshed when a partner re-inits (`claude-init-session`), and a send-time id mismatch
**re-binds** rather than breaking. The fork's auto-resume command is *not* `--fork-session`
(that would re-fork on every resume) — once the fork has its own id, `handleEnableAutoResume`
drops the fork flag so it resumes its own conversation like any Claude tab.

**Injection:** bracketed paste (`ESC[200~ … ESC[201~`) + a deferred `\r` so multi-line
messages stay one prompt and submit cleanly into Claude's TUI.

## Mesh Workspace (N:M agent bridging)

A **Mesh Workspace** (`Workspace.bridge_all = true`) generalizes the 1:1 bridge: every agent
tab in it is reachable by every other, over **topic-scoped** threads, with **no broadcast**
(each message is crafted for one recipient). Full design: `docs/mesh-workspace.md`. Phase 1
is headless (agents run in normal splits); the stage/filmstrip view is Phase 2.

**Layered for testability** (each layer unit-tested, no Svelte/Tauri in the cores):
- `src/lib/stores/agentDelivery.ts` — the recipient-keyed FIFO mailbox, **shared** with the
  1:1 bridge (the mesh constructs its own controller instance).
- `src/lib/stores/meshRouting.ts` — recipient resolution + the topic registry. Routing keys
  off the **stable tabId handle**, never the editable role name (rename can't misroute);
  ambiguous/unknown recipient → hard error with the roster (never a silent drop). Topics are
  first-class, deduped by a **normalized label** (mirrors Rust `MeshTopic::normalize_label`);
  create-on-first-send (sender owns), owner-or-human completes, completed topics **reject**
  further sends at the tool boundary.
- `src/lib/stores/meshSend.ts` — the send orchestration: build envelope with the next turn,
  deliver, then commit topic state on success; emit a conversation edge **only on delivered**
  (phantom-edge guard). `failed` commits nothing.
- `src/lib/stores/agentMesh.svelte.ts` — the live store: derives the roster from workspace
  membership (a **named** agent tab in a `bridge_all` workspace), wires the cores to live
  state, persists the topic registry, and drives member readiness off the same hook events as
  the 1:1 bridge (`agent-init-session`/`agent-hook-stop`/`agent-hook-session-end`).

**Persistence:** `Workspace.bridge_all` + `Workspace.mesh_topics: Vec<MeshTopic>` (both
`serde(default)`). Commands `set_workspace_bridge_all` and `set_workspace_mesh_topics`
(coarse whole-registry replace — the frontend store is authoritative; the persist command
re-canonicalizes each `normalized_label` server-side as an integrity guard). Roster is
**derived, not persisted** (closing a tab removes it; renaming changes only the display
label). Tests: `meshRouting.test.ts` (18), `meshSend.test.ts` (8), Rust `mesh_topic_tests` (4).

**MCP tools:** `sendToBridgedAgent` gains optional `recipient` + `topic` (required-by-context
at runtime in a mesh, untouched for 1:1 — Codex #1); `listBridgedPeers`, `listTopics`,
`startTopic`, `completeTopic`. Dispatch in `claudeCode.svelte.ts` routes to `agentMeshStore`
when the tab is in a mesh workspace, else the 1:1 `agentBridgeStore`.

## Claude Code Hooks Integration

Hooks registered in `~/.claude/settings.json` on MCP server startup, cleaned up on app exit and stale lockfile sweep.

**Hooks registered:**
- `SessionStart` (command): Echoes tab ID into Claude's context. Gated on `$MAITERM_PORT` matching server port (prevents dev/prod cross-talk). Output appears collapsed in TUI ("Ran 1 start hook") but injected into model context as system-reminder.
- `SessionStart` (HTTP): POST to `/hooks` with `{session_id, cwd, source, model}`. Registers session→tab mapping in `AppState.agent_sessions`.
- `SessionEnd` (HTTP): Removes session from mapping.
- `Notification` (HTTP): Receives Claude Code notification events.
- `Stop` (HTTP): Receives stop events.

**Connection tab affinity (`initSession`):**
- Claude calls `initSession({ tabId, sessionId })` as its first MCP tool call
- Server stores connection_id → tab_id mapping in `ServerState.connection_tabs`
- All subsequent tool calls on that connection auto-inject `tabId` if missing
- Prevents wrong-tab targeting when user switches tabs while Claude is working
- Connection affinity cleaned up on disconnect (WS close, SSE drop)

**Streamable-HTTP connection identity (the load-bearing part for local agents):** local
Claude connects over `type: http` (`POST /mcp`), which has no persistent socket — so
`connection_id` rides entirely on the `Mcp-Session-Id` header. The server **assigns** a
fresh uuid on the `initialize` handshake and returns it in the response header
(`derive_streamable_connection_id` in `server.rs`); the client echoes it on every later
request, giving each agent a unique `mcp-<uuid>` key. **This is critical for Agent
Bridge:** the old code never assigned an id and fell back to one shared constant
`"streamable-http"` for ALL sessionless requests, so two local agents collapsed onto a
single affinity key. Since `initSession` is the only writer of that key, the last agent
to init silently owned it and the other agent's tool calls resolved to the WRONG tab —
the "bridge dropped" report, "fixed" only by re-running `/maiterm init` (which re-claimed
the shared slot until the peer re-claimed it). Never reintroduce a shared sessionless
key; mint a per-request id instead so distinct agents can't merge.

**SSE reconnect recovery:** SSE connections over SSH tunnels flap frequently (disconnect/reconnect every few seconds). Each reconnect creates a new SSE session ID, clearing the old `connection_tabs` entry. Without recovery, every tool call after a reconnect fails with "Session not initialized." Fix: when a tool call arrives with no connection affinity, `agent_sessions` is checked for active sessions. Recovery only binds when unambiguous — exactly one active session, or (with multiple bridged agents) exactly one of them currently lacks a live connection. With 2+ ambiguous candidates it declines and requires an explicit `initSession`, because guessing could bind one agent's call to another agent's tab (the same class of cross-agent corruption as the shared-key bug above).

**Dev/prod isolation:**
- PTY env vars: `MAITERM_TAB_ID` (tab ID), `MAITERM_PORT` (server port) — set at spawn in `pty/manager.rs`
- Command hook gates on `$MAITERM_PORT` match
- MCP tool guard in `server.rs` rejects `tabId` that doesn't exist in this instance
- MCP instructions specify server name (`maiterm` vs `maiterm-dev`)

**`/maiterm` skill (auto-installed):**
- Written to `~/.claude/skills/maiterm/SKILL.md` on startup, removed on exit (`write_aiterm_skill` / `remove_aiterm_skill` in `lockfile.rs`)
- Provides fast slash-command access: `/maiterm notes`, `/maiterm diag`, `/maiterm tabs`, etc.
- Reduces LLM inference by giving explicit tool→parameter mappings
- **`init` fast-path** lives at the top of the SKILL.md: it tells the agent the exact deferred-tool lookup (`ToolSearch select:mcp__maiterm__initSession,mcp__maiterm-dev__initSession,mcp__aiterm__initSession,mcp__aiterm-dev__initSession`) instead of a broad keyword search across every connected MCP server — this is what keeps `/maiterm init` cheap. Build-agnostic (covers both dev and prod keys; the `aiterm`/`aiterm-dev` names are legacy fallbacks kept during the rename rollout), so no interpolation.
- **`statusline` subcommand** is the one entry that is NOT an MCP tool — it installs the maiTerm status line by running bundled helper scripts under `~/.claude/skills/maiterm/bin/`:
  - `setup-statusline.sh` — renders a colored example, then either installs (exit 0) or reports missing `jq` (exit 3, prints `JQ_MISSING:<cmd>`, writes nothing). Idempotent: only touches `~/.claude/statusline-command.sh` + the `statusLine` key in `~/.claude/settings.json`.
  - `statusline-command.sh` — the status line itself (host · cwd · git branch · model · effort · context %).
  - **Single source:** both scripts live in `src-tauri/resources/maiterm-skill/bin/`, baked into the binary via `include_str!` (`STATUSLINE_SETUP_SCRIPT` / `STATUSLINE_PAYLOAD_SCRIPT`) and exposed to the frontend via the `get_maiterm_skill_scripts` command so the remote (SSH) install reuses the exact same bytes.
- **Single SKILL.md source:** the `/maiterm` skill body lives once at `src-tauri/resources/maiterm-skill/SKILL.md`, baked in via `include_str!` (`MAITERM_SKILL_MD` in `lockfile.rs`) for the local install and shipped to the remote (SSH) install through `get_maiterm_skill_scripts` → `MaitermSkillScripts.skill_md` (consumed by `sshMcpBridge.svelte.ts::buildSetupScript`). Edit the resource file only; both installs track it. The trailing `$ARGUMENTS` is the slash-command placeholder.

**Stale hook cleanup:** On startup, `write_hook_settings()` sweeps hooks whose port has no live lockfile. `cleanup_stale_lockfiles()` also removes hooks for dead servers by auth token. Port extraction handles both URL format (`127.0.0.1:NNNNN`) and the command-hook env-var format (`MAITERM_PORT = "NNNNN"`, whose marker substring also matches legacy `AITERM_PORT` hooks).

**Hook self-heal:** the 30s reassert loop (`reassert_if_drifted`) covers BOTH `~/.claude.json` (`ensure_mcp_settings`) and the hooks in `~/.claude/settings.json` (`ensure_hook_settings`, gated on `claude_hooks` pref). Both files are co-owned: the `claude` CLI rewrites them, and an SSH-bridge setup script that lands in a local shell clobbers them with remote-tunnel ports (dead locally → ECONNREFUSED on every hook). `build_our_hooks()` is the single definition shared by install and drift check. Drift also includes **stale foreign maiTerm entries** (hook or allowlist ports with no live lockfile) — since the sweep lives in `write_hook_settings()`, which only runs on drift, `hooks_are_current()` must return false when a dead foreign port is present, or every session dials it forever (ECONNREFUSED on every hook event). The canonical source of such entries: **this machine was the ssh target of a peer maiTerm's bridge** — the peer's remote setup legitimately writes hooks for its reverse-tunnel port plus a `pid: 0` lockfile here; when the tunnel dies those hooks go dead. Lockfile liveness must go through `lockfile_is_live()`: pid > 0 → process check; pid 0 (tunnel lockfile — the listener is sshd, not a process we can see) → TCP probe of the port. Never call `is_process_alive(0)`: `kill(0, 0)` signals our own process group and always succeeds, which made tunnel lockfiles immortal.

**Auto-open notes panel:** `claudeCode.svelte.ts` auto-opens notes panel when MCP tools write tab notes or workspace notes, switching scope as appropriate.

## SSH MCP Bridge (Remote IDE Tools)

Exposes local MCP tools to Claude Code running on remote servers via SSH reverse tunnels.

**Architecture:**
```
Local maiTerm → SSH reverse tunnel (-R 0:127.0.0.1:{mcp_port}) → Remote :allocated_port
               Background SSH → writes lockfile + ~/.claude.json on remote
Remote Claude Code → discovers ~/.claude/ide/{port}.lock → connects through tunnel → local MCP server
```

**Key files:**
- `src-tauri/src/commands/ssh_tunnel.rs` — tunnel lifecycle (start, detach, kill), port parsing, `ssh_run_setup` for background lockfile writing
- `src/lib/stores/sshMcpBridge.svelte.ts` — bridge orchestration, reactive status tracking, ref counting

**Preference:** `claude_code_ide_ssh` (default true, requires `claude_code_ide`). Controls auto-enable on SSH detection.

**Tunnel sharing:** One tunnel per `host_key` (user@host), ref-counted by tab IDs. Last tab detaches → tunnel killed.

**Auto-enable:** SSH sessions detected reactively via terminal title changes — when a title change fires, `getPtyInfo()` checks for a foreground SSH command and enables/disables the bridge accordingly. For restore/clone SSH: polls `getPtyInfo()` every 500ms until SSH is detected (max 15s). `enableBridge` sets a `'pending'` state immediately to prevent race conditions from concurrent title-change calls.

**Remote setup:** Lockfile, `~/.claude.json`, hooks (`~/.claude/settings.json`), skill (`~/.claude/skills/maiterm/SKILL.md` + `bin/` statusline helper scripts, fetched via `get_maiterm_skill_scripts`), and `~/.aiterm` env file are written via a separate background SSH connection (`ssh_run_setup`), **not** through the user's interactive PTY. This prevents command injection into running programs (e.g. Claude Code). The setup script uses shell variables for JSON data to avoid nested quoting issues, and pipes JSON to python3/jq via stdin. After setup, `MAITERM_TAB_ID` and `MAITERM_PORT` env vars are injected into the remote shell via PTY write (leading space suppresses shell history).

**`~/.aiterm` env file:** Written during bridge setup with `export MAITERM_TAB_ID=... MAITERM_PORT=...`. Sourced as a fallback by the SessionStart hook (and the Codex `agent-hook.sh` shim) when `$MAITERM_TAB_ID` is empty (e.g. inside tmux where env vars weren't inherited). Users can manually `source ~/.aiterm` in any shell. **Sole-tab gated:** the file is per-ACCOUNT, but all tabs on one host share ONE reverse tunnel/port, so an env-less agent on a shared account can't be disambiguated — a stale file would hand it whichever tab connected most recently, corrupting session/tab identity (the wrong tab gets the session registered + `claudeSessionId` + auto-resume repointed). So `buildSetupScript` writes it only when this maiTerm is the *sole* bridged tab on that host (`isSharedHost(hostKey, tabId)` false); on shared hosts it runs `rm -f ~/.aiterm` (also scrubbing stale pre-fix files) and env-less agents fail closed to a visible "needs init" rather than silently mis-registering.

**Context menu items (SSH tabs with active bridge):**
- "Inject maiTerm Env Vars" — re-writes `export MAITERM_TAB_ID=... MAITERM_PORT=...` to the PTY for the current shell (useful after tmux attach, sudo, su)
- "Install MCP for Current User" — writes the full setup script (lockfile, MCP, hooks, skill) to the PTY, executing as the current user. Needed after `sudo -i` or `su -l otheruser` where `~/` changed but the tunnel is still accessible on localhost.

**Remote hooks:** All hook events (SessionStart, SessionEnd, Notification, Stop, UserPromptSubmit, PreToolUse, PostToolUse, PreCompact) are registered on the remote with HTTP hooks pointing to `127.0.0.1:{remotePort}/hooks`. These tunnel back through the SSH reverse tunnel to the local MCP server's hooks handler. A command hook on SessionStart reads `$MAITERM_TAB_ID` (from env var injection) and echoes the tab ID into Claude's context. Hooks require python3 on the remote for the settings.json merge.

**Remote cleanup:** Stale lockfile detection on reconnect tests dead ports via `/dev/tcp/localhost/{port}`. No EXIT trap (background SSH has no persistent shell on remote). Stale hooks with dead port URLs are NOT silent — Claude Code prints `hook error / connect ECONNREFUSED` in every session until they're removed. On an ordinary remote they linger until the next bridge setup rewrites them; when the "remote" is itself a maiTerm machine, its own hook self-heal sweeps them (tunnel lockfiles have `pid: 0`, so liveness is the port probe in `lockfile_is_live()`).

**Port allocation:** `ssh -v -R 0:...` lets SSH pick a free remote port. The `-v` flag is required because ControlMaster mux clients print nothing without it. Port parsed from both stdout and stderr (direct connections use stderr, mux clients use stderr with `-v`). Uses `tokio::select!` to read both streams concurrently.

**ControlMaster mux:** The tunnel is the **master of a maiTerm-owned socket** (`~/.maiterm/cm[-dev]/<host_key>.sock`, see `cm_socket_path` in `ssh_tunnel.rs`) — never of the user's `~/.ssh/master-*` namespace (a maiTerm connection owning the user's socket once broke their own `ssh <host>` when it died: "Session open refused by peer"). No `ControlPersist`, so the socket lives and dies with the tunnel process we track by pid; ssh unlinks it on master exit and `cleanup_cm_socket` covers kills/crashes, plus a stale-file sweep before each spawn (a leftover file makes `ControlMaster=yes` silently degrade to non-mux). Short-lived maiTerm clients — SSH transcript-mirror fetches (`mailink/mirror.rs`), future scp image sends — mux over it with `ControlMaster=no` for ~tens-of-ms re-auth-free commands, falling back to independent BatchMode connections when the socket is dead. Setup commands (`ssh_run_setup`) stay fully independent (`ControlPath=none`). Windows: no ControlMaster support — the tunnel runs as a plain independent connection.

**Bridge status UI:** Reactive `$state` Map in `sshMcpBridge.svelte.ts` drives a bolt icon in TerminalTabs (green=connected, dim/fg-dim=pending, dim=failed). Failure dispatches an in-app notification via `notificationDispatch`.

## SSH-Specific Pitfalls

- **SSH ControlMaster mux silent output**: When SSH multiplexes through an existing master socket, `ssh -R 0:...` prints nothing to stdout or stderr without `-v`. The "Allocated port" message only appears with verbose mode. Additionally, the mux client exits immediately with code 0 after setting up the forwarding — the master process holds the tunnel. Background tunnel monitors must not clean up state on clean exit.
- **SSH background command quoting**: Shell commands sent via `ssh user@host 'script'` must use newlines (not `;`) as separators — `do;`, `then;`, `else;` are syntax errors. JSON data should be stored in shell variables and passed to python3/jq via stdin to avoid nested quote hell.
