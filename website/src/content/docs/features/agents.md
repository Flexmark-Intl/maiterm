---
title: Agent Integration
description: One runtime-neutral pipeline that wires Claude Code and OpenAI Codex into maiTerm — IDE tools, live agent state, auto-resume, and bridging.
---

maiTerm integrates deeply with coding agents — **Claude Code** and **OpenAI Codex** — through a single runtime-neutral pipeline. Both connect to the same MCP/IDE server, drive the same live agent-state indicators, auto-resume after a restart, pair through [Agent Bridge](/features/agent-bridge/), and send notifications — all without a manual setup step. maiTerm detects which agent connected from its own identity, so the right integration just lights up the moment you run `claude` or `codex`.

## Supported agents

| Agent | On by default | Integration |
|-------|---------------|-------------|
| **Claude Code** | Yes | MCP/IDE tools, hooks, auto-resume, Agent Bridge (fork or connect), SSH bridge, `/maiterm` skill + status line |
| **Codex** | Yes | MCP/IDE tools, lifecycle hooks, auto-resume, Agent Bridge (connect, or fork — new), SSH bridge, `maiterm` prompt |

Both agents get the same core treatment: live state in the sidebar and footer, tab activity indicators, auto-resume after a crash or relaunch, and notifications — all driven through the same hooks pipeline. Integration is on by default for each; it only takes effect once you actually run that agent.

## How It Works

```
Claude Code / Codex CLI ←→ Streamable HTTP ←→ axum server (Rust) ←→ Tauri events ←→ Frontend (Svelte)
```

There's **one** MCP/IDE server, shared by every agent. It starts automatically when maiTerm launches and listens on a single port with a single auth token. What differs per agent is only the on-disk config a CLI reads to discover it:

- **Claude Code** — a lock file in `~/.claude/ide/`, an `mcpServers` entry in `~/.claude.json`, and lifecycle hooks in `~/.claude/settings.json`.
- **Codex** — an MCP block in `~/.codex/config.toml`, lifecycle hooks in `~/.codex/hooks.json`, and a `maiterm` prompt in `~/.codex/prompts/`.

When an agent connects, maiTerm identifies the runtime from the client's own handshake — so a Codex connection never binds to a Claude tab. And **no agent needs a manual registration step**: a tab names itself on the wire, and the session links itself the moment the agent process starts. See [Acting as the right tab](#acting-as-the-right-tab).

## Choosing your agents

Agent settings live in one runtime-neutral **AI Agents** section in Preferences, with a subsection per agent. Toggling an agent on or off installs or removes its integration immediately — no restart required.

**Claude Code**

- **Enable IDE Integration** — the MCP/IDE server and tools
- **Enable Hooks Integration** — lifecycle hooks for real-time state
- **Enable Auto-Resume via Hooks** — capture session IDs and reconnect on restore
- **Enable IDE Integration over SSH** — expose IDE tools to remote Claude Code via reverse tunnel

**Codex**

- **Enable Codex IDE integration** — the MCP/IDE server and tools for Codex
- **Codex lifecycle hooks** — the 9 Codex hook events that drive state and auto-resume. Turning this off removes the hooks rather than just leaving them unrefreshed, so Codex stops reporting to maiTerm
- **Codex auto-resume** — capture session IDs and reconnect on restore
- **Codex MCP bridge over SSH** — expose IDE tools to remote Codex
- **Skip the one-time Codex hook-trust prompt** *(advanced)* — the only agent toggle off by default; Codex's one-time hook-trust approval is deliberately kept unless you opt out

### SSH MCP Bridge

When you're SSH'd into a remote server, maiTerm bridges the MCP connection so an agent running remotely still has access to all IDE tools. A reverse SSH tunnel is set up automatically in the background — no manual port forwarding needed. For each enabled agent maiTerm writes the matching remote config (Claude Code's lock file and `~/.claude.json`, or Codex's `~/.codex/config.toml` and `hooks.json`), gracefully no-op'ing on a host that doesn't have that CLI installed. The bridge status is shown in the tab bar with a bolt icon (green = connected).

If a tunnel goes down — a network blip, or your Mac putting its displays to sleep — maiTerm rebuilds it on its own, backing off between attempts and staggering the tabs that share a host so they rejoin one connection rather than all re-authenticating at once. A tab whose shell has since `ssh`'d somewhere else follows the shell instead of being reconnected to the host it left.

#### Two computers, one remote account

The remote configuration files live in the account's home directory, one copy shared by every session on that host — so if they named a particular maiTerm's connection, whichever machine connected last would own the account and every agent belonging to the other one would silently lose its tools and its hooks. A laptop and a desktop are enough to hit that; so are your own dev and release builds.

Those files now name no maiTerm at all. Each agent works out which one to talk to from **its own terminal session's environment**, which maiTerm sets per tab when it opens the connection — so every machine writes identical bytes and there is nothing left to fight over. maiTerm also asks the remote host for a fixed port of its own rather than taking whatever it's given, and a tab that was told the old number is corrected as soon as its tunnel is up.

It now keeps that port, too. Tunnels are closed when maiTerm exits, and anything an earlier run left behind is cleared out at startup instead of squatting the port and pushing the next launch onto a different number. If you have more than one copy of maiTerm running — a dev build beside a release one — the cleanup only ever removes connections nobody owns, never the live ones belonging to the other copy.

One consequence worth knowing: a remote shell that **maiTerm didn't start itself** — one you opened inside `tmux`, or after `su`, or by typing `ssh` in another window — has none of that environment and can't reach maiTerm at all. (Before, it would reach *some* maiTerm, often the wrong one.) The remedy depends on how many tabs you have open to that host:

- **One tab on the host** — maiTerm leaves the values in `~/.aiterm`, so `source ~/.aiterm` in the stray shell is enough.
- **Several tabs on the same host** — that file is deliberately *removed* rather than written. Every tab on a host shares one tunnel, so a shell with no environment of its own can't be told apart from its siblings, and a fallback file would quietly hand it another tab's identity. Start the shell from a maiTerm tab instead.

`claude mcp list` names what's missing if you're not sure which case you're in.

## Available Tools

Both agents connect to the same MCP server and can call the same tools.

### Editor Tools

| Tool | Description |
|------|-------------|
| `getOpenEditors` | List open editor tabs (path, language, dirty state) |
| `getWorkspaceFolders` | Workspace root paths |
| `getDiagnostics` | Language diagnostics for a file |
| `checkDocumentDirty` | Check if file has unsaved changes |
| `saveDocument` | Save file to disk |
| `getCurrentSelection` | Active editor selection + cursor |
| `getLatestSelection` | Most recent selection in any tab |
| `openFile` | Open file in editor tab (with optional line/text selection) |
| `openDiff` | Show side-by-side diff for review (blocking — accept/reject) |
| `showDiff` | View a git diff read-only (non-blocking) |
| `closeAllDiffTabs` | Close all pending diff tabs |

### Workspace & Tab Navigation

| Tool | Description |
|------|-------------|
| `listWorkspaces` | List all workspaces with panes, tabs (IDs, display names, types, active state, notes) |
| `switchTab` | Navigate to a tab by ID (auto-resolves workspace and pane) |
| `getTabContext` | Get recent terminal output or editor content for tab discovery |

### Notes Management

| Tool | Description |
|------|-------------|
| `getTabNotes` | Read notes for a tab (defaults to active tab) |
| `setTabNotes` | Write or clear notes for a tab |
| `listWorkspaceNotes` | List workspace-level notes (IDs, previews, timestamps) |
| `readWorkspaceNote` | Read full content of a workspace note |
| `writeWorkspaceNote` | Create or update a workspace note |
| `deleteWorkspaceNote` | Delete a workspace note |
| `moveNote` | Move notes between tab and workspace (with conflict detection) |
| `openNotesPanel` | Open, close, or toggle the notes panel |
| `setNotesScope` | Switch notes panel between tab and workspace views |

### Tab State & Preferences

| Tool | Description |
|------|-------------|
| `getActiveTab` | Get the currently active workspace, pane, and tab info |
| `setTriggerVariable` | Set or clear a trigger variable for a tab |
| `getTriggerVariables` | Read all trigger variables for a tab |
| `setAutoResume` | Enable/disable auto-resume with optional command/cwd/ssh overrides |
| `getAutoResume` | Get current auto-resume configuration for a tab |
| `getPreferences` | Read maiTerm preferences |
| `setPreference` | Update an maiTerm preference |
| `findNotes` | Search all tabs and workspaces for notes in one call |
| `getDiagnostics` | App diagnostics — version, PTY stats, memory, WebGL state |
| `readLogs` | Tail the log file with level filter and search |
| `getClaudeSessions` | List all active agent sessions across tabs with state, tool, and model info |
| `listWindows` | List all maiTerm windows with workspace summaries |
| `createBackup` | Create a state backup on demand |
| `sendNotification` | Send a toast or OS notification from your agent |

### Agent Bridge

| Tool | Description |
|------|-------------|
| `sendToBridgedAgent` | Send a message to the peer agent this tab is bridged with — async, the reply arrives as a new prompt turn |
| `getBridgedAgent` | Report whether this tab is bridged and, if so, the partner's label and working directory |

See [Agent Bridge](/features/agent-bridge/) for the full feature.

### Tasks

| Tool | Description |
|------|-------------|
| `listTasks` | List the project's tasks, grouped by workstream — this tab's, or the whole workspace |
| `createTasks` | Create a batch of tasks, optionally into a named workstream |
| `updateTasks` | Update a batch — status, title, detail, workstream, blockers |

Three batched tools over one list you and your agent both edit. There is deliberately no delete tool — an agent may mark a task done, only a human removes one. See [Tasks](/features/tasks/) for the full feature; it can be switched off entirely in **Preferences → AI Agents → Task tracking**, which removes both the tools and the instruction that goes with them.

### Send files to your phone

| Tool | Description |
|------|-------------|
| `sendFilesToPhone` | Send files from this machine to your paired [maiLink](/features/mailink/) phone |

Files land in that tab's chat on the phone and in a cross-chat **Files** list. Any file type, up to 1 GB each; each path succeeds or fails on its own, so one bad path doesn't sink the batch. An agent on an SSH tab passes its own remote paths and maiTerm fetches them back over the bridge tunnel. The bytes are copied at the moment of sending, so rewriting the file afterwards can't retroactively change what you were already given.

### Supervision

If [Overlord](/features/overlord/) is enabled and this window has an Overlord workspace, every supervised agent also gets `replyToOverlord`, and the supervisor agent gets a set of tools of its own — driving another tab, answering a prompt it's stuck at, proposing rule changes, and putting a finished session away. See [Overlord](/features/overlord/).

### Acting as the right tab

Every tool call an agent makes has to be attributed to the tab it's running in — that's what makes `getTabNotes` read *your* notes and `switchTab` mean something. maiTerm establishes that two ways, both automatic:

- **A tab names itself on every request.** maiTerm sets `MAITERM_TAB_ID` in each tab's environment, and the agent's own MCP configuration carries it as a header on every call it makes. Nothing is printed into the model's context and nothing is typed by anyone.
- **A SessionStart hook links the session.** It runs inside the tab's own shell — so it can see which tab it is — and registers the agent's session id the moment the agent process starts, which is what wires up auto-resume and hands the agent its standing instructions.

Between them, a restart, a resume, a fork or a compaction all come back correctly bound with nothing typed. That last part used to be the sharp edge: a resumed agent takes no turn until you type something, so before this there was no moment at which it could announce itself, and every restored tab sat unregistered — no tools, no hooks, invisible to maiLink and the mesh — until you noticed and re-registered it by hand.

The two sources are cross-checked rather than trusted blindly. The session is durable and wins on *which* tab; the header is exact but only as good as the shell that launched the agent, so it raises confidence when the two agree. When they disagree, or when only a deduction is available — an SSH connection re-mints its transport every few seconds, and a shell started inside `tmux` or after `su` inherits somebody else's environment — the identity is treated as **inferred**, and the tools whose effect *leaves* the tab are refused on one: messaging or listing [bridge](/features/agent-bridge/) peers, [mesh](/features/mesh-workspace/) topics, posting to a [Mattermost](/features/comms/) thread, and [sending files to your phone](#send-files-to-your-phone). Those speak under the tab's name into someone else's session, where a wrong guess can't be taken back. The refusal says so and names the fix.

#### `/maiterm init` is a repair tool now

It still exists, and it still works — but it is no longer a startup step, and agents are told not to spend an opening turn on it. Run it when identity is genuinely wrong: a stale `MAITERM_TAB_ID` inherited from another shell, or an inferred-identity refusal like the one above. Everything else takes care of itself.

### Tab Context Discovery

The `getTabContext` tool lets your agent peek at what's happening in your tabs — recent terminal output or editor file content. If you have fewer than 10 tabs, it automatically returns context for all of them, making it easy for the agent to find the right tab without you having to specify. For larger workspaces, you can pass specific tab IDs.

## Agent Hooks

maiTerm integrates with each agent's hook system for real-time session awareness — no regex triggers needed. Claude Code's hooks are registered in `~/.claude/settings.json`; Codex's lifecycle events are registered in `~/.codex/hooks.json`. Both feed the same pipeline:

- **Session lifecycle** — tracks session start, end, and compaction events
- **Active tool overlay** — see what the agent is doing right now (editing files, running bash, etc.) in the terminal corner
- **Agent state indicators** — per-tab, per-workspace, and a global footer dot show whether each agent is working, waiting for permission, or done — see [Agent State Indicators](#agent-state-indicators) below
- **Auto-resume** — automatically captures session IDs and reconnects on tab restore (see [Auto-resume](#auto-resume) below)
- **Multi-agent awareness** — `getClaudeSessions` lets any session discover other active agent sessions across tabs for coordination, and [Agent Bridge](/features/agent-bridge/) lets two sessions talk to each other directly
- **Compaction notifications** — alerts during and after context compaction

## Agent State Indicators

maiTerm surfaces what every agent is doing at three levels, all driven by hooks — no terminal-output guessing:

- **Per tab** — each tab's indicator reflects its agent: a pulse while working, ❗ when it needs permission, and a green dot when it's done and waiting for input. Ordinary terminal output stays a dim dot, so a finished agent is never mistaken for a stray line of output.
- **Per workspace** — the sidebar rolls a workspace's tabs into one dot using batch semantics (`permission > working > done`). It turns green only once *every* agent in the workspace has settled, so green unambiguously means "all done."
- **Global footer dot** — rolls up every agent in the current window: red pulse = needs permission, accent pulse = working, green = finished, dim = no agents. Click it to jump straight to an agent's tab; when several agents share the dominant state, each click cycles to the next.

### Read vs. unread

A finished agent shows a **filled** green dot (unread); once you view its tab it relaxes to a **hollow** green ring (seen). This rolls up too — a workspace dot stays filled until every finished agent inside it has been seen, then goes hollow — so you can tell at a glance which completed agents still need a look.

## Auto-resume

The moment an agent's session starts, maiTerm captures its session ID and arms auto-resume so a restored or relaunched tab reconnects to the same conversation. Auto-resume is **runtime-aware**: the resume modal preselects the right command for whichever agent is running — `claude --resume …` for Claude Code, `codex resume …` for Codex — and hides itself entirely when a tab has no agent.

## Agent Bridge

Bridge two running agent sessions so they can collaborate directly — one local and one over SSH, two related projects, even a Claude Code and a Codex. Press `Cmd+Shift+L` to pick a peer session; maiTerm forks it into a split pane beside you (when the target supports forking) or links an existing tab, and the two agents talk through `sendToBridgedAgent`. Every message is stamped with the sender's real identity so the recipient knows it's a peer, not you — and the agents stay deferring to you for decisions. Bridges persist across restarts.

See the dedicated [Agent Bridge](/features/agent-bridge/) page for the full walkthrough. To scale past a single pair to a whole roster of agents that can each address any other by role — with topics, loop control, and a cockpit — see [Mesh Workspace](/features/mesh-workspace/), the N:M generalization of a bridge.

## maiLink — answer from your phone

The same hooks that drive the state indicators can ring your phone. [maiLink](/features/mailink/) is a companion app that connects directly to maiTerm over your LAN — encrypted and authenticated, with no cloud in the data path — so you can watch live transcripts, answer `AskUserQuestion` prompts, approve permissions, and interrupt a running agent from anywhere in the house. See the dedicated [maiLink Companion](/features/mailink/) page.

## File Drop & Image Paste

Drag files onto a terminal running an agent over SSH — maiTerm SCP uploads them to a temp directory on the remote and pastes the paths so the agent can read them as file references. On local terminals, file paths are pasted directly.

You can also paste images from your clipboard (Cmd+V) into an agent session. maiTerm saves the image to a temp file and pastes the path, so the agent can view it directly — useful for sharing screenshots, diagrams, or error messages without leaving the terminal.

## The `/maiterm` Skill

For Claude Code, maiTerm installs a skill that gives your agent fast slash-command access to the most common tools — `/maiterm notes`, `/maiterm tabs`, `/maiterm diag`, and more — without making it hunt through every MCP tool. It's written on launch and removed on exit, and it works the same way over SSH on bridged remote hosts. Codex gets an equivalent `maiterm` prompt written to `~/.codex/prompts/`.

It also ships a recommended **status line** for Claude Code. Run `/maiterm statusline` and your agent installs a compact status line showing host · cwd · git branch · model · reasoning effort · context usage. The installer renders a live colored preview first, only writes to `~/.claude/statusline-command.sh` and your settings (it's idempotent — safe to re-run), and tells you if `jq` is missing instead of leaving a broken line. It works on local sessions and on SSH-bridged hosts.

## Dev/Production Isolation

Dev builds register as `maiterm-dev` (with display name "maiTermDev"), so development and production instances don't interfere with each other.
