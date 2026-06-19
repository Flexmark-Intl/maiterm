# Claude Code IDE Integration

maiTerm exposes an MCP server that Claude Code CLI discovers and connects to, providing IDE-like capabilities.

## Architecture

```
Claude Code CLI ←→ WebSocket/SSE ←→ axum server (Rust) ←→ Tauri events ←→ Frontend (Svelte)
```

**Backend** (`src-tauri/src/claude_code/`):
- `server.rs` — axum router with WebSocket (`/`) and SSE (`/sse` + `/message`) endpoints. Random port (10000–65535), 32-char auth token.
- `protocol.rs` — JSON-RPC request/response types, `tool_list_response()` (46 tools), `initialize_response()`
- `lockfile.rs` — writes `~/.claude/ide/{port}.lock` for discovery, registers `mcpServers.maiterm` (or `maiterm-dev`) in `~/.claude.json` (stripping the legacy `aiterm`/`aiterm-dev` key on write — rebrand migration), registers hooks in `~/.claude/settings.json`

**Frontend** (`src/lib/stores/claudeCode.svelte.ts`):
- Listens for `claude-code-tool` Tauri events
- Dispatches to tool handlers (getOpenEditors, openFile, openDiff, etc.)
- Responds via `claude_code_respond` Tauri command

**Enabled by**: `preferences.claude_code_ide` (default true). Server startup is two-phase: `prepare_server()` runs synchronously inside Tauri's `setup()` (binds the TCP port, writes `~/.claude.json`, hooks, skill), then `serve_server()` runs as a background tokio task that adopts the pre-bound listener and runs `axum::serve`. The sync prep is load-bearing — frontend doesn't load (and no PTY can auto-resume `claude --resume …`) until `~/.claude.json` has the current port.

## Tools Exposed

| Tool | Description |
|------|-------------|
| initSession | **REQUIRED first call.** Registers tab ID + session ID → enables auto-inject of tabId on all subsequent calls |
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
- `SessionStart` (command): Echoes tab ID into Claude's context. Gated on `$AITERM_PORT` matching server port (prevents dev/prod cross-talk). Output appears collapsed in TUI ("Ran 1 start hook") but injected into model context as system-reminder.
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
- PTY env vars: `AITERM_TAB_ID` (tab ID), `AITERM_PORT` (server port) — set at spawn in `pty/manager.rs`
- Command hook gates on `$AITERM_PORT` match
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

**Stale hook cleanup:** On startup, `write_hook_settings()` sweeps hooks whose port has no live lockfile. `cleanup_stale_lockfiles()` also removes hooks for dead servers by auth token. Port extraction handles both URL format (`127.0.0.1:NNNNN`) and legacy env var format (`AITERM_PORT = "NNNNN"`).

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

**Remote setup:** Lockfile, `~/.claude.json`, hooks (`~/.claude/settings.json`), skill (`~/.claude/skills/maiterm/SKILL.md` + `bin/` statusline helper scripts, fetched via `get_maiterm_skill_scripts`), and `~/.aiterm` env file are written via a separate background SSH connection (`ssh_run_setup`), **not** through the user's interactive PTY. This prevents command injection into running programs (e.g. Claude Code). The setup script uses shell variables for JSON data to avoid nested quoting issues, and pipes JSON to python3/jq via stdin. After setup, `AITERM_TAB_ID` and `AITERM_PORT` env vars are injected into the remote shell via PTY write (leading space suppresses shell history).

**`~/.aiterm` env file:** Written during bridge setup with `export AITERM_TAB_ID=... AITERM_PORT=...`. Sourced as a fallback by the SessionStart hook when `$AITERM_TAB_ID` is empty (e.g. inside tmux where env vars weren't inherited). Users can manually `source ~/.aiterm` in any shell. Overwritten on each bridge connect — self-correcting for stale values.

**Context menu items (SSH tabs with active bridge):**
- "Inject maiTerm Env Vars" — re-writes `export AITERM_TAB_ID=... AITERM_PORT=...` to the PTY for the current shell (useful after tmux attach, sudo, su)
- "Install MCP for Current User" — writes the full setup script (lockfile, MCP, hooks, skill) to the PTY, executing as the current user. Needed after `sudo -i` or `su -l otheruser` where `~/` changed but the tunnel is still accessible on localhost.

**Remote hooks:** All hook events (SessionStart, SessionEnd, Notification, Stop, UserPromptSubmit, PreToolUse, PostToolUse, PreCompact) are registered on the remote with HTTP hooks pointing to `127.0.0.1:{remotePort}/hooks`. These tunnel back through the SSH reverse tunnel to the local MCP server's hooks handler. A command hook on SessionStart reads `$AITERM_TAB_ID` (from env var injection) and echoes the tab ID into Claude's context. Hooks require python3 on the remote for the settings.json merge.

**Remote cleanup:** Stale lockfile detection on reconnect tests dead ports via `/dev/tcp/localhost/{port}`. No EXIT trap (background SSH has no persistent shell on remote). Stale hooks with dead port URLs are harmless (fail silently) and cleaned up on next bridge setup.

**Port allocation:** `ssh -v -R 0:...` lets SSH pick a free remote port. The `-v` flag is required because ControlMaster mux clients print nothing without it. Port parsed from both stdout and stderr (direct connections use stderr, mux clients use stderr with `-v`). Uses `tokio::select!` to read both streams concurrently.

**ControlMaster mux:** Tunnel and setup SSH commands do **not** use `-o ControlMaster=no` — this lets them multiplex over the user's existing authenticated socket (free auth for password/passphrase users). Mux clients exit immediately after setup (the master holds the forwarding), so the background process monitor only removes tunnel state on error exits, not clean exits.

**Bridge status UI:** Reactive `$state` Map in `sshMcpBridge.svelte.ts` drives a bolt icon in TerminalTabs (green=connected, dim/fg-dim=pending, dim=failed). Failure dispatches an in-app notification via `notificationDispatch`.

## SSH-Specific Pitfalls

- **SSH ControlMaster mux silent output**: When SSH multiplexes through an existing master socket, `ssh -R 0:...` prints nothing to stdout or stderr without `-v`. The "Allocated port" message only appears with verbose mode. Additionally, the mux client exits immediately with code 0 after setting up the forwarding — the master process holds the tunnel. Background tunnel monitors must not clean up state on clean exit.
- **SSH background command quoting**: Shell commands sent via `ssh user@host 'script'` must use newlines (not `;`) as separators — `do;`, `then;`, `else;` are syntax errors. JSON data should be stored in shell variables and passed to python3/jq via stdin to avoid nested quote hell.
