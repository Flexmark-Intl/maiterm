# Changelog

## v1.20.0

- **Resuming a workspace brings back exactly the agents that were running.** Suspending a workspace now remembers which tabs had a live terminal; resuming it respawns and auto-resumes just those — so a 20-tab workspace that had 3 agents running comes back with those 3 live (with a progress modal for larger resumes), instead of waking tabs you never started or leaving live ones dead.
- **maiLink — companion app refinements.**
  - **Transcripts now show up for remote and Codex agents.** SSH-hosted Claude tabs — whose transcript lives on the remote host — and Codex tabs were rendering blank in the phone inbox; both now fall back to the live session so you see the conversation. Codex conversations distill per-turn from Codex's own session log, with the model name and a context-window gauge, matching how Claude tabs already worked.
  - **Questions from your agents are answerable again, with the right timing.** An AskUserQuestion now reliably rings on the phone and shows what's being asked, and its countdown reflects whether your Claude Code build actually expires the question (newer builds leave it open by default) — so a live question no longer looks expired, and a late answer to a question that already closed can't accidentally land on the next one.
  - **Permission cards show what you're approving.** A permission prompt now reads like `Bash(rm -rf ./dist) — approve?` instead of just the tool name, and approving a Codex prompt sends the keystroke that matches Codex's variable-length approval list, so your choice can't select the wrong option.
  - Attention alerts (the doorbell) fire only on a real transition into "needs you," so merely opening the app or restoring a session no longer pushes a phantom "finished," while an AskUserQuestion that opens without a permission prompt still rings.
  - Cleaner transcripts: agent-to-agent mesh and bridge messages are filtered out instead of flooding every participant's thread, a fan-out of subagents shows each one's task instead of a run of identical `Agent` chips, and an injected screenshot no longer leaves a duplicate raw-path bubble when you re-open the thread.

## v1.19.0

- **maiLink — reach your agents from your phone.** A companion app that connects **directly to maiTerm running on your own computer** so you can watch and steer your agents from anywhere in the house. It talks to maiTerm over your **LAN only**, on an encrypted, authenticated channel (TLS with a per-device paired token) — **no cloud service sits in the middle**. The one small exception is a content-free push "doorbell": when an agent needs you while the app is backgrounded, a tiny relay hosted on Cloudflare wakes the app, carrying no transcript or message content. To reach your agents from outside your network, set up a WireGuard VPN back to your LAN rather than exposing anything to the internet.
  - **Live transcripts, streamed per turn.** Each agent's conversation streams over a WebSocket as it happens, with a per-agent meta strip showing the model and a context-window gauge, and context compaction shown as a divider.
  - **Answer prompts from the phone.** An agent's AskUserQuestion arrives as a native prompt you can answer — single-select, multiSelect, and "Other" free-text all work — and you can respond to or interrupt a running agent. The inbox sorts by real per-tab activity, and answering a question clears its card immediately.
  - **Secure pairing you control.** Pair a device by scanning a QR code; paired devices are listed and individually revocable, each with its own token, so revoking one phone never touches the others. The relay is multi-tenant with capability auth — no shared secret.
  - **Send images to a local session.** Attach images to a message and they're injected straight into the local Claude Code session.
  - Designate which tabs and workspaces are reachable from maiLink — default-on with a per-tab exception — under a new Preferences section.
- **Full-session restore is now solid.** A tab's PTY is authoritative for whether it's actually live, fixed by a one-time boot reconcile and a sequential relaunch — so after a crash or update, tabs come back actually running instead of looking live but dead, and a mesh auto-recheck now waits until restore finishes before acting.
- **Mesh polish.** The three loop-control caps default to off, so a mesh never pauses a conversation unless you opt in. Long agent names wrap in the cockpit graph instead of overlapping. Agents report to you only through native AskUserQuestion — status-note scraping is gone — and an AskUserQuestion raises a deterministic "needs you" toast.
- **New tabs inherit where you were.** Opening a new tab adopts the previous tab's host and working directory (falling back to the suspended majority), so it lands where you were working instead of your home directory.
- Fix Claude chat history silently not persisting after a deploy or restart — a leaked `CLAUDE_CODE_*` environment marker made a resumed session write to a child transcript that was never saved. The markers are now scrubbed when a terminal spawns.
- Harden the SSH MCP bridge on a flaky remote: a bridge that fails is retried instead of wedging forever, and a hung tunnel fails fast and is reaped rather than hanging the connection.
- Fix workspace-note writes auto-opening the notes panel, and scope an agent's workspace-note operations to its own workspace.
- Run shutdown cleanup on a native (Cmd+Q or window-close) quit, not just a menu quit, so background work is torn down cleanly however you exit.

## v1.18.0

- **Mesh Workspace — connect a whole workspace of agents, not just two.** The Agent Bridge linked exactly two agents; a Mesh Workspace generalizes that to N:M. Turn on a workspace as a mesh and every named agent tab in it can message any other by role name, with no broadcast — each message is addressed, routed off a stable handle so a rename can never misroute, and an unknown or ambiguous recipient is a hard error with the roster, never a silent drop.
  - **Topics keep multi-agent conversations organized.** Messages group into topics owned by whoever starts them; a topic dedupes by normalized label, and only its owner — or you — can complete it. Completed topics reject further messages, so a thread can't quietly reopen.
  - **Loop control stops runaway ping-pong.** A soft per-topic turn cap pauses a back-and-forth at a checkpoint (a resume lifts it by another round), a hard ceiling is an absolute backstop a resume can't clear, and a time-to-live force-pauses a stale topic — so two agents left alone can't burn tokens or thrash terminals indefinitely.
  - **The cockpit drawer is your control surface.** Open it with Cmd+Shift+M or the MESH badge on a mesh workspace. It shows a live conversation graph (agents on a circle, topic "stars" weighted by turn count, a pulse on topics that just delivered), a topic list with human complete/resume controls, and a status board parsed from each agent's workspace note — a "NEEDS DECISION" block raises a deep-linked toast so you know exactly when to step in. Click any node to jump to that tab.
  - **Stage + filmstrip view.** Swap a mesh workspace's split tree for a focused two-panel stage with a live, scaled filmstrip of the other agents. Click a tile to bring an agent to the left panel, Shift+click for the right; an Exit button returns to the normal layout. Built on the existing portal rendering, so promoting and demoting agents never reflows or respawns a terminal.
  - **Pre-flight setup and readiness.** Enabling a mesh opens a readiness modal that inventories every tab — ready, not-yet-registered, suspended (with Wake), or unnamed (with inline rename) — so a mesh comes up with exactly the agents you intend. After a restart it auto-rechecks and offers to wake or re-init any agent that dropped, and agent purposes you set now persist across restarts.
- **Full-session restore on relaunch.** maiTerm now respawns and auto-resumes every workspace's active tab on launch — a crash, update, or quit-and-relaunch comes back as it was, not just the last-active workspace (new "Restore session" preference; the old last-active-only behavior is still available). A window reload reattaches to the still-running backend terminals instead of killing and respawning them, so reloading never loses live output. A workspace now looks active only when one of its tabs actually has a live terminal, so clicking around without resuming anything no longer makes everything look running.
- **Pin tabs to the front of the strip.** Pinned tabs cluster at the start of the tab bar and stay put — they're exempt from the active/suspended regrouping that shuffles other tabs around.
- **Smoother under load.** Terminal output is now coalesced to ~60fps with a guaranteed trailing frame, so several agents streaming at once — or hidden agent tabs in the same workspace — no longer saturate the renderer and bog down the whole app. A parked terminal still echoes a keystroke with zero added latency.
- Pasted screenshots are encoded as JPEG instead of lossless PNG, shrinking multi-megabyte clipboard images to a fraction of the size; images with transparency still use PNG to keep the alpha channel.
- Fix dropped files pasting a local path instead of uploading to an SSH or agent session on faster remote hosts — the drop now waits for the SSH probe before routing, so uploads work regardless of drag speed or host latency.
- Fix a production-only renderer crash when a workspace with splits churned its layout (add/remove/collapse a pane, or switch workspaces), which could crash-loop the renderer and make the UI appear frozen.
- Fix SSH file listing reporting a failure on remote directories that have no `.env` file (the common case).
- Fix Agent Bridge occasionally delivering a message ahead of older queued ones when a hold cleared between drain ticks — delivery is now strictly oldest-first.

## v1.17.1

- Fix agent prompts not submitting on large or multi-line pastes, image sends, and long agent-to-agent bridge replies. Claude Code and Codex collapse a big paste into a `[Pasted text #N]` / `[Image #N]` chip via async processing, and a submit keystroke arriving before that settled was swallowed — the prompt would stage in the input but never send. The composer and the Agent Bridge now share one paste→settle→submit sequence with a payload-scaled delay, so short typed lines still send instantly while large payloads wait just long enough.
- Add a search box to the archived-tabs dropdown, matching the hidden-tabs dropdown — an auto-focused field that filters archived tabs by name as you type.

## v1.17.0

- **Codex support — run OpenAI's Codex CLI as a first-class agent.** maiTerm's deep agent integration is no longer Claude-only. Codex now gets the same treatment: live state in the sidebar and footer (working / waiting / finished), tab activity indicators, auto-resume after a crash or relaunch, Agent Bridge pairing, and notifications — all driven through the same hooks pipeline that powers Claude Code. On SSH hosts, maiTerm installs and configures remote Codex over the bridge just as it does for Claude, and it works without a manual `/maiterm init` because the runtime is detected from the hook path. Codex integration is on by default; it only takes effect once you actually run Codex.
- **New "AI Agents" preferences section.** Agent settings are consolidated into one runtime-neutral section where you choose which agents — Claude Code, Codex — maiTerm integrates with locally and over SSH. Toggling an agent on or off installs or removes its integration immediately, no restart required. The Agent Bridge picker is now runtime-neutral, so you can pair any two supported agents.
- **Auto-resume is now runtime-aware.** The auto-resume modal preselects the right resume command for whichever agent is running and hides itself when the tab has no agent. The standalone "Auto-resume + Claude" setup-guide modal has been removed in favor of this.
- Fix wide emoji and CJK glyphs rendering as half-glyphs. The terminal now uses Unicode 11 character widths, so two-cell characters occupy the correct number of columns.
- Always surface `.env` files in Quick Open, with a clearer show/hide toggle.
- Context menus now clamp inside the window and scroll when taller than the available space, so items never spill off-screen.
- Fix the footer agent dots' click target collapsing to the size of the dot — the full indicator is clickable again.

## v1.16.1

- **Agent activity shows as three footer dots.** The sidebar's agent indicator was a single dot that only showed the highest-priority state, so a waiting agent could mask others that were still working or already finished. It's now up to three independent dots — working (blue), waiting (red), finished (green) — each clickable to cycle through every agent in that state. The sidebar is a touch wider (215px) to fit them while keeping workspace names readable; narrower saved widths snap up automatically.
- Fix bridged agents not receiving messages mid-task. Messages meant for a busy agent were held until it fully stopped, so a missed stop signal could wedge them indefinitely — while a permission prompt (the one moment we must not interrupt) was incorrectly treated as deliverable. Messages now arrive whether the agent is working or idle and are held only for a human prompt, matching how Claude Code captures mid-turn input.
- Fix the Agent Bridge breaking when a bridged tab was closed or reloaded. Closing a bridged tab left its partner pointing at a ghost, and Cmd+Shift+R reload orphaned the link entirely (one tab kept the ⇄ icon, the other lost it). Bridges now tear down cleanly on close and transfer across a reload, and a tab whose partner went away becomes immediately re-bridgeable without a restart.
- Fix composer sends not submitting when files or screenshots are attached. The submit keystroke was swallowed while Claude Code asynchronously loaded the attachment paths; it's now sent as a separate keystroke after a short settle window. Text-only sends are unchanged.
- Fix dropped or pasted files with spaces in their name not resolving to attachments in a Claude session that maiTerm hadn't yet recognized as Claude. Composer attachments now always send the literal file path, matching the terminal's own drag-and-drop convention.
- Fix Cmd+Shift+R reload of an SSH tab ignoring an auto-resume remote directory you'd explicitly set, reconnecting to the live session's current directory instead. A configured auto-resume SSH command and remote directory now take priority on reload.

## v1.16.0

- **Move tabs between panes without cloning.** Three new ways to rearrange your split layout — all move the existing tab (PTY intact, no respawn) instead of duplicating it. Drag a tab onto another pane's tab bar to drop it there; drag onto a pane's body to move into the center zone or create a new split on the edge you drop on (left, right, top, or bottom). For keyboard-first use, right-click any tab for "Move to New Split Right/Down" and per-pane move items — the first tab-level context menu in maiTerm.
- **Image viewer background switcher.** Transparent PNGs were invisible under dark themes. The image viewer toolbar now has a toggle that cycles Dark → Light → Checkerboard, so you can inspect transparency regardless of your active theme. The choice persists across sessions.
- **Resumed tab lands next to the tab you came from.** Unsuspending a tab from the tab strip or the hidden-tabs dropdown now places it right after the tab you were in, instead of at the end of the active group — so it's always one tab away when you switch back.
- Agent Bridge picker defaults to connecting an existing tab instead of creating a new one — the more common case when linking two already-running agents.
- Fix scrollback duplication where TUI apps (Claude Code, Ink-based CLIs) left permanent duplicate blocks in scrollback after a terminal resize. The PTY now coalesces rapid resize events and background tabs spawn at their saved size, so a width change never lands mid-stream.

## v1.15.0

- **Composer dock — write prompts like a human.** A per-tab, autogrowing multi-line input docked below the terminal, built for composing long prompts for Claude Code (or any CLI) without fighting the shell's single-line editing. Enter inserts newlines, Cmd+Enter sends, and Esc returns focus to the terminal. When the foreground app has bracketed paste on (Claude Code, zsh, modern readline) the whole block is sent as one literal paste that submits once; otherwise lines are sent raw. Drafts persist per tab across switches and restarts, and Cmd+Shift+C toggles the dock. It's on by default (configurable under Tabs)
- **Attach files and screenshots to a prompt as chips.** Paste a copied screenshot or Finder files, or drop files onto the open dock, and they appear as chips above the input instead of raw paths — with image thumbnails, dedup, and one-click removal. On send, the paths are appended to your text (raw for Claude sessions, shell-escaped for plain shells); for SSH sessions the files are uploaded to the remote host first and referenced by their remote path
- **Suspended tabs show their last session behind the resume overlay.** Instead of a blank panel, a suspended terminal now displays its previous scrollback behind a frosted-glass resume overlay, so you can see what the tab held before bringing it back
- **Edit Markdown table cells in place in notes.** In the rendered notes preview, click a table cell to edit it directly — Enter or blur commits, Esc cancels, and Tab/Shift+Tab move between cells. Only the edited cell's bytes are rewritten, so the rest of the document stays byte-identical
- **Cmd+G goes to a line number in the editor.** While the search panel is open Cmd+G stays find-next; otherwise it opens go-to-line

## v1.14.0

- **Restore on Relaunch is now on by default.** maiTerm reopens your terminal sessions and scrollback when you relaunch, so you pick up exactly where you left off. It switches on automatically once; if you'd previously turned it off — or turn it off now — that choice sticks and is never silently re-enabled
- **Agent Bridge picker is now searchable and sorted by recent activity.** When you create a bridge, candidate agents are listed most-recently-active first, with a search box to filter by tab name, workspace, or path — much faster when you have many agents running. It also tells you when an agent isn't listed yet (an agent only appears once it has registered with maiTerm, i.e. made a tool call or run `/maiterm init`). The terminal context-menu item is now "Create Agent Bridge…"
- Fix Agent Bridge dropping over and over between two agents on the same machine. Local agents connect over an HTTP transport that, without a per-session id, made them all share a single connection slot — so whichever agent acted most recently "owned" it and the others' tool calls landed on the wrong tab, which an agent reported as the bridge being dropped. Each agent now gets its own session id, so a bridge stays pointed at the right peer without having to re-run `/maiterm init`
- Fix two bridged agents deadlocking when one queued a message for the other mid-task. A queued message could sit undelivered when the recipient went idle waiting on the sender, leaving both agents stuck waiting on each other. Queued messages now drain as soon as the recipient is free, in both directions
- Fix the tab strip scrolling to the wrong place when you resume a suspended tab — it no longer snaps to the slot the tab held before it was promoted into the active group
- With "group active tabs" on, a tab now joins the active group on its first manual unsuspend instead of requiring a second one
- Internal: the MCP server Claude Code connects to is now keyed `maiterm` (was `aiterm`) in `~/.claude.json`, so the agent's tool list and status reflect the current name. Existing integrations keep working — the old key is migrated automatically

## v1.13.3

- **Search and per-tab age in the hidden-tabs menu.** The overflow menu for tabs that scroll out of view now has a search box to filter by name and shows how long each suspended terminal has been idle, matching the archive list. The popup is wider and taller so longer names fit
- **Clicking into a terminal, editor, or diff now focuses its pane.** Pane-targeted actions like Cmd+T (new tab) and Cmd+D (split) operate on the pane you last clicked into — not just the last pane whose header you clicked — so they go where you're actually working
- Fix Agent Bridge reporting a dropped bridge and misrouting messages when two or more agents are active at once. With multiple live agents, an unbound connection could be matched to the wrong agent's tab, making a bridged agent see its own tab as its partner. Tab resolution now only auto-recovers when it's unambiguous, and a bridge can never point at itself
- Fix a restored archived tab briefly landing next to the active tab and then jumping to the end of the active group a second later when "group active tabs" is on — the restored tab now keeps the position it was given
- Claude agents running in maiTerm now consistently refer to the app as "maiTerm"; the remaining "aiTerm" strings in the MCP tool descriptions and agent-facing messages have been updated. Your data, settings, and integrations are unaffected

## v1.13.2

- **New macOS 26 "Liquid Glass" app icon.** On macOS Tahoe (26), the dock and Finder icon now use Apple's adaptive Liquid Glass treatment — a periwinkle glass tile with the maiTerm "m" that the system renders for the light, dark, tinted, and clear appearances automatically. On earlier macOS, Windows, and Linux the existing icon is unchanged. The documentation site's logo and favicon now match the new icon as well

## v1.13.1

- **Install the recommended Claude Code status line with `/maiterm statusline`.** One command sets up a compact status line showing host · current directory · git branch · model · reasoning effort · context-used %, so you get the same at-a-glance Claude Code context line we use. It's idempotent (safe to re-run) and works both locally and on SSH-bridged hosts. `/maiterm init` is also faster now — it uses a targeted tool lookup instead of scanning every connected MCP server
- The "What's New" changelog modal now renders Markdown, so **bold**, *italic*, `code`, and links in release notes display formatted instead of showing their literal markup
- Fix Windows release downloads being published with an empty version in their filename (a double-dash `maiterm--windows-…`) — the version-detection step ran under PowerShell instead of bash, so it produced no version

## v1.13.0

- **aiTerm is now maiTerm.** New name, new look: a refreshed app icon and wordmark, theme-aware light/dark logos throughout the app, and a cleaner title bar that shows just the active workspace name with the maiTerm mark on the right. Your data carries over untouched — the app's underlying identifier is unchanged, so every workspace, tab, note, preference, and scrollback buffer is exactly where you left it, and the existing settings/state directories are reused as-is. No re-setup, no migration step
- The Claude Code slash command is now `/maiterm` instead of `/aiterm` (e.g. `/maiterm notes`, `/maiterm diag`, `/maiterm init`). The old `/aiterm` skill is removed automatically on first launch so you won't be left with a stale duplicate, and the underlying MCP server keeps its name — so Claude Code's IDE integration continues working without any reconnect or reconfiguration
- **Add Agent Bridge** — connect two running Claude agents in different panes so they can talk to each other directly. Link the active tab to another Claude session (Cmd+Shift+L, or the terminal context menu → "Link to Agent…"), and the agents exchange messages as real prompt turns in each other's panes, so you see the whole conversation and can interrupt with Esc at any time. Two modes: fork a session into a new split pane (an isolated peer that inherits the original's full context), or link two tabs that are already open. Each message is stamped with the sender's identity so an agent never mistakes a peer for you, links persist across an app restart, and a broken link self-heals after auto-resume
- Detect unexpected SSH disconnects: when a remote session drops, the tab keeps its title and shows a one-click reconnect instead of silently falling back to a local shell with no indication anything happened
- Add live upload progress with a cancel button when sending files to a remote host over SCP, replacing the previous silent wait with no feedback
- Add an overflow menu for tabs that scroll out of view, placed between the new-tab and notes buttons, so older tabs in a busy pane stay one click away
- Add an anonymous update-check counter (no personal data collected) so we can gauge how many installs are active
- Fix a file-descriptor leak on macOS where the editor's file watcher could gradually exhaust the process's available file descriptors over a long session, eventually causing failures opening files or spawning new shells
- Fix local editor tabs not refreshing when the underlying file was changed on disk by something outside the app

## v1.12.8

- Fix a new terminal tab almost always opening in the wrong directory in workspaces that have accumulated many suspended tabs. A new tab inherits the most common working directory (and SSH setup) among its sibling tabs, but the tally counted *suspended* tabs too — and a suspended tab carries the stale directory it was last in. In a long-lived workspace where most suspended tabs sat in the same place, that majority always won, so every new tab opened there regardless of which tab you were actually on. The tally now counts only live tabs, so a new tab follows the tab you opened it from. Live SSH tabs also now contribute their real remote directory (from the shell prompt) instead of a stale or local-only path
- When "group active tabs" is enabled, resuming a suspended tab now moves it into the active group's order, not just visually. Previously a resumed tab jumped to the front of the tab bar on screen but kept its old stored position, so the visible order and the real order disagreed (and a drag would snap it back). The resumed tab now settles at the end of the active group — where it already appears — so dragging within your active tabs behaves predictably and the tabs you've most recently used stay together at the front, even after everything is suspended again

## v1.12.7

- Fix a hard freeze where the main window could become visible but completely unresponsive — no typing or clicking — after a display/monitor change (docking, undocking, or a monitor sleeping). Trackpad-scrolling over a full-screen terminal app (a TUI like `less`, `vim`, or a pager) while the terminal had been refit with a zero-height layout made xterm.js's scroll math divide by a zero row-height, producing an "infinite" scroll distance; it then built an unbounded escape-key string in a tight loop, pinning a CPU core at 100% and growing that window's renderer process to ~14 GB until it ran out of memory. Only the affected window's renderer was wedged — other windows and the backend kept working, which is also why the in-app diagnostics looked healthy (they sample the main process, not the per-window renderer). aiTerm now ignores wheel events on a terminal that hasn't been laid out yet, so the runaway can't start; normal scrolling and mouse-aware TUIs are unaffected

## v1.12.6

- Fix terminal rendering artifacts that showed up under heavy output from Claude Code: red diff-line backgrounds smearing across unrelated rows, and a staircase of half-typed or duplicated input (along with stray text from earlier) when typing while an agent was streaming. The v1.12.2 WebGL→Canvas switch fixed glyph ghosting, but the Canvas renderer turned out to ghost too under aiTerm's workload — the backend streams a full-viewport repaint (clear-screen + full content) ~60 times a second, and the GPU-backed backbuffer doesn't fully overwrite the previous frame, so stale cells linger until enough repaints accumulate. The underlying terminal grid was always correct; only the renderer was wrong, which is why the mess eventually cleared itself up. Switched the default to xterm's built-in DOM renderer, which replaces each cell outright and so can't ghost — aiTerm only ever renders a single bounded viewport (scrollback:0), so the GPU renderers' throughput advantage never applied here anyway
- Add a **Terminal → Rendering** preference to choose the renderer (DOM or Canvas). DOM is the new default; Canvas remains available for side-by-side comparison. Changing it applies immediately to visible terminals

## v1.12.5

- Fix the global "X agents working" footer dot doing nothing when clicked if the dominant agent lived in a *different* window. The dot rolled up Claude sessions globally, but Claude-hook events broadcast to every window — so each window's session map held agents from all windows, while click-to-cycle only searches the current window's tabs and silently fell through when the target lived elsewhere. Every window also showed the same global count instead of its own agents. The rollup is now scoped to the current window's tabs, so each window's dot is independent and every cycle target is reachable

## v1.12.4

- Fix Claude Code's IDE tools (notes, diagnostics, session tracking — the whole `aiterm` MCP toolset) silently breaking partway through a long session. aiTerm registers its MCP server in `~/.claude.json` at startup, but that file is co-owned by the `claude` CLI, which rewrites the whole file on its own events — a long-lived CLI session holding a stale in-memory copy could clobber aiTerm's entry, leaving Claude Code dialing a dead port for the rest of the session (MCP tool calls would hang, then error). aiTerm now re-asserts its entry on a 30s timer, so a clobber self-heals within one tick. The check is read-only and idempotent — it only rewrites the file when the entry has actually drifted, so there's no added disk churn. Also replaced a stale-tab-ID error that could bounce a session in circles between the dev and prod instances with a deterministic recovery path

## v1.12.3

- Make the global Claude-agent footer dot cycle through agents. When more than one agent is in the dominant state (e.g. "3 agents working"), clicking the dot used to always jump to the same representative tab. It now advances to the next matching agent on each click — anchored on the tab you're currently viewing and wrapping around — so repeated clicks walk through every agent. The tooltip gains a "(click to cycle)" hint when more than one is listed
- Fix the SSH MCP bridge being torn down when a terminal tab carrying a live PTY is moved between workspaces. Moving the tab preserves the PTY, but the bridge was being shut down anyway, breaking Claude Code's IDE integration on that session until reconnect
- Allow selecting and copying text in the rendered (preview) notes view — previously only the raw markdown editor allowed selection
- Trim trailing periods from detected file-path links so a path at the end of a sentence no longer swallows the period into the clickable link

## v1.12.2

- Fix terminal glyph ghosting — stale, overlapping glyphs that showed up on Claude Code spinners, diffs, and bold text. The cause was the xterm.js WebGL renderer compositing redrawn cells *over* the previous frame (its backbuffer is alpha-blended even though the terminal is opaque) instead of opaquely replacing them, so only redrawn cells ghosted and a refit cleared it. Switched the renderer from WebGL to Canvas, which clears each cell opaquely before drawing and so can't ghost — WebGL's scroll-perf advantage never applied here since aiTerm renders a single bounded viewport (scrollback:0). Falls back to xterm's built-in DOM renderer if the Canvas addon throws
- Replace the sidebar footer's renderer status dot with a global Claude-agent indicator that rolls up agent state across *all* workspaces: red pulse = needs permission, accent pulse = working, green = finished & unread, hollow ring = all seen, dim = no agents. Click it to jump to a representative agent tab
- Fix editor scroll-jump: a long file scrolled to the bottom via the scrollbar would jump back ~a screenful and drop the cursor on the wrong line when clicked. The browser's native "scroll the caret into view on focus" was yanking the viewport back to the old caret before CodeMirror mapped the click. Clicks now pre-focus the content with scrolling suppressed so the click maps to the correct line, and releasing the scrollbar without clicking restores the user's scroll position

## v1.12.1

- Fix recently-changed state being lost when you install an update. The auto-updater's "Install & Restart" relaunches the app by hard-killing the process, which skipped the normal shutdown save path — so anything not yet flushed to disk (most visibly a just-renamed tab, which would revert to its `%title`, plus scrollback and window geometry) was discarded. The updater now saves window geometry, scrollback, and workspace state before relaunching
- Fix tab renames living only in memory until some later save happened to flush them — renaming a tab now persists immediately

## v1.12.0

- Add a read/unread state to the Claude agent-done indicators. When an agent finishes, its tab shows a filled green dot (unread); once you view the tab it becomes a hollow green ring (seen). This is rolled up to the workspace sidebar too — the workspace dot stays a filled green dot until *every* finished agent in it has been seen, then goes hollow. Lets you tell at a glance which completed agents you still need to look at
- Add an hourly background check for app updates so a long-running window notices new releases without a restart. The check is silent (only surfaces the update banner/toast if one is found) and respects the "automatically check for updates" preference

## v1.11.0

- Add a workspace-level agent-state indicator to the sidebar, driven by Claude Code hooks (#2). The rolled-up workspace dot now mirrors the per-tab indicators — blue pulse while any agent is working, green once every agent in the workspace is done (waiting for input), and ❗ when an agent needs permission. Generic terminal output is demoted to a dim dot so a finished agent is no longer indistinguishable from any other line of output. Aggregation uses batch semantics (`permission > active > idle`): the dot only turns green when the whole workspace has settled, so green unambiguously means "done"
- Fix back/forward history navigation, and add a Window > Clear Back/Forward History menu item
- Fix `moveTabToWorkspace` ignoring grouped active tabs when choosing the new active tab after a move
- Fix the crash-marker warning log being dropped because it was emitted before the logger was initialized

## v1.10.8

- Drop the `/aiterm init` slash-command argument from the default Claude auto-resume command — recent Claude Code releases became unreliable about running the slash command on `--resume`, which is the dominant use case. The SessionStart hook (local and SSH) already tells Claude to call `initSession` on every new, resumed, forked, or compacted session, so the extra argument was redundant. Existing tabs with the old template form (including the legacy `claude --resume <interpolated-uuid> "/aiterm init"` variant from older releases) are auto-migrated to the new form on startup and on archived-tab restore

## v1.10.7

- Fix QuickOpen (double-Alt) trying to list files over SSH on a local terminal that had merely *used* ssh earlier in the session (e.g. Claude Code running ssh via its Bash tool). SSH-vs-local detection now reads the controlling tty's foreground process group (tpgid) and only reports ssh when the pgid leader at that pid is itself an ssh/mosh/autossh process — subprocesses inherit the foreground app's pgid but aren't what the user is interacting with. Side note: `sudo ssh host` and `bash -c "ssh host"` are no longer auto-detected (leader is sudo/bash, not ssh)
- Fix SSH MCP bridge slot leak when a tab is suspended: suspend kills the PTY but doesn't unmount TerminalPane, so `onDestroy` never fired and `disableBridge()` was being skipped — the shared `ssh -L` tunnel kept the suspended tab in its refcount

## v1.10.6

- Add post-crash forensics for WebKit renderer crashes: a running-marker file is refreshed each minute and cleared on graceful exit, so the next launch can detect that the previous run died uncleanly (`previous_run.crashed` + `marker_mtime_secs` in `getDiagnostics`)
- Scan `~/Library/Logs/DiagnosticReports/` (and Retired/) for matching aiTerm and `com.apple.WebKit.WebContent` crash dumps from the last 30 days; surface process, exception type, and termination reason via `getDiagnostics.crash_reports`
- Capture unhandled webview errors and promise rejections to `aiterm.log` tagged `[WEBVIEW_ERROR]`, so JS errors that immediately precede a renderer crash are no longer silent
- Fix tab strip scroll position on workspace restore (active tab is now scrolled into view)

## v1.10.5

- Move scheduled backup timer from the webview to a Rust background task — backups now keep firing even if the main window's frontend hangs (which previously stopped the setInterval that drove them)
- Persist the diagnostics memory trend to disk (`aiterm-memory-trend.json`) and sample RSS every 60 seconds in the background, capped at 12 hours of history; the buffer is reseeded from disk on startup so post-mortem analysis after a freeze still has the RSS curve leading up to it
- Stop mutating the memory trend ring buffer as a side effect of `getDiagnostics` — reads are now pure and don't perturb the data being analyzed

## v1.10.4

- Guard state save against stale/zombie aiTerm processes overwriting newer data — disk mtime is checked before every save, and conflicting writes are preserved as `aiterm-state.conflict-<ms>.json` instead of clobbering the live state
- Skip the Cmd+W two-press confirm for editor and diff tabs (only terminal tabs require the second press)
- Expand getDiagnostics to expose JS heap, DOM node count, internal store map sizes, trigger engine buffers, and a per-event Tauri listener leak counter

## v1.10.3

- Send SSE keepalives on the MCP stream to prevent SSH idle disconnects (30s–3min drops)
- Register MCP port in ~/.claude.json before setup() returns (fixes auto-resume race)
- Reset Term before feeding restored scrollback (fixes duplicated scrollback after restart)
- Preserve nav forward history when diverting mid-walk
- Make Cmd+W close hint more visible (centered card on dimmed/blurred backdrop)

## v1.10.2

- Require two presses for Cmd+W to close a tab (prevents accidental close with armed 2s overlay)
- Prune orphan scrollback rows from SQLite DB on startup, close_window, reset_window, and import_state
- Ref-count Claude Code IDE connection state to dampen SSE reconnect flap (reduces IPC churn)
- Document editor fold shortcuts and two-press Cmd+W in help page

## v1.10.1

- Add Cmd+Shift+- / Cmd+Shift+= to fold all / unfold all in editor
- Parallelize SSH MCP bridge env-var injection with remote setup (~0.5-2s faster)
- Skip SSH MCP bridge for one-shot remote commands
- Preserve transparency when pasting clipboard image into Claude session (PNG instead of JPEG)
- Navigate to most recent non-suspended tab in nav history on workspace suspend
- Fix nav history walk losing position when closing walked-to tab
- Fix Cmd+Shift+[/] jumping to stale tabs by centralizing history push in setActiveTab

## v1.10.0

- Add suspend tab button that kills PTY while keeping tab + scrollback visible
- Add macOS Full Disk Access detection and Permissions section in Preferences
- Rewrite nav history as unique-per-tab MRU with separation from tab cycling
- Replace goto-line footer with centered modal (line or line:col)
- Show %claudeSessionId with copy button in auto-resume edit modal
- Focus editor view when editor tab becomes visible
- Skip suspended tabs when cycling with keyboard shortcuts
- Fix MCP tab-notes handlers misrouting on tab switch mid-call
- Fix new-tab inheritance using stale PTY state over pinned auto-resume
- Fix drag-drop not detecting Claude over SSH, improve drop overlay visibility
- Fix auto-resume pinned settings lost on tab reload, restore, and copy
- Fix suspend-tab deleting the tab instead of showing resume prompt
- Fix new workspace showing resume prompt on first tab
- Fix closing editor/diff tab navigating to wrong tab on first open

## v1.9.1

- Add Go to Line (Ctrl+G) and improve editor toolbar visibility
- Add gitignore toggle, tooltips, and draggable palette to Quick Open
- Fix terminal selection coordinates drifting during PTY output (scrollback rotation)
- Fix editor scroll jump when using scrollbar and auto-reload scroll reset
- Fix tab close returning to wrong tab when group-active-tabs is enabled
- Fix new terminal tabs flashing into suspended group before PTY registers

## v1.9.0

- Add Quick Open file search palette (double-press Alt/Opt or Cmd+P) with fuzzy matching, glob patterns, and SSH remote support
- Add directory navigation in Quick Open (Tab to enter, Backspace to go back, dotfile toggle)
- Add recently-opened and mtime-sorted file ordering in Quick Open
- Convert workspaces store to Svelte 5 direct mutations, fixing notes panel reverting edits during terminal output

## v1.8.6

- Re-check for newer version before installing update (choice prompt if a newer release appeared)
- Add openFile in-place tab replacement (targetTabId) and SSH-aware file opening via SCP
- Fix remote image preview blocked by CSP missing img-src data: directive

## v1.8.4

- Notes panel dynamic max width (caps at 90% of pane width instead of hardcoded 600px)
- Fix resume gate for duplicate/reload/split tabs and all-suspended overlay resume
- Fix Cmd+O file dialog rejecting webp/image/PDF files
- Fix SSH auto-resume failing due to leading space in remoteCwd
- Fix MCP bridge falsely activating during SCP/rsync/git file transfers
- Fix drag-drop SCP upload toast and echo for non-Claude SSH sessions
- Fix horizontal overflow clipping on markdown tables in notes panel

## v1.8.3

- Fix resume gate excluding duplicate/reload/split tabs via splitContext check

## v1.8.2

- Fix suspended terminal tabs auto-activating when previous tab is closed (resume gate now covers all activation paths)
- Fix nav history (Cmd+[/]) navigating to suspended tabs without live PTY
- Fix group-active-tabs effect causing surprise tab jumps on every active tab change

## v1.8.1

- Add archived tab tools (list/restore) to MCP server
- Add skill commands to /aiterm (switch, open, windows, archived, restore, prefs, backup)
- Make notification toasts clickable by passing tab source for navigation
- Fix MCP protocol macro recursion limit by splitting tool definitions into batches

## v1.8.0

- Add clipboard image paste support for Claude Code sessions
- Add file deletion detection for editor tabs (auto-close deleted files, clean nav history)
- Add resume gate for suspended tabs and fix cross-window preference sync
- Add install button to What's New modal and update check toasts
- Always emit claudeSessionId on initSession regardless of auto-resume setting
- Reduce MCP server log noise by downgrading chatty messages to debug

## v1.7.16

- Fix tab deletion race during workspace suspension (guard teardown with suspendingWorkspaceIds)
- Add Cmd+[/] back/forward navigation to help shortcuts

## v1.7.15

- Add browser-style back/forward tab navigation (Cmd+[/]) with cross-workspace history stack
- Fix notes heading sizes and reduce default notes font size to 13

## v1.7.14

- Tab bar UX overhaul: scrollable tabs with pinned archive/new-tab/notes buttons
- Add "Group active tabs first" preference to visually separate live from suspended tabs
- Add "Move to workspace notes" button in tab notes panel
- Add clipboard image paste support for Claude sessions (temp JPEG, SCP for SSH)
- Restored tabs now insert after the active tab instead of at position 0
- Fix blank lines in git status output (control chars in renderer causing line wraps)

## v1.7.13

- Replace update toast with persistent sidebar banner (Install/Restart buttons, stays until dismissed)
- Add "What's New" link that fetches missed release notes from GitHub API

## v1.7.12

- Add editTabNotes MCP tool for precision note edits (single or batch, sequential matching)

## v1.7.11

- Fix MCP session loss on SSE reconnect with multiple active Claude sessions (track connection_id for orphan detection)

## v1.7.10

- Fix SSH detection failing due to ps output parsing bug (collapsed whitespace splitting)
- Add Windows process introspection for SSH detection via sysinfo
- Fix auto-resume SSH/CWD context loss on disable/re-enable cycle (fall back to stored values)

## v1.7.9

- Add auto-updater: check for and install updates from GitHub Releases with toast-based UX
- Add "Check for Updates" menu item in aiTerm and Help menus
- Add auto-check on startup preference (Preferences > Updates)

## v1.7.8

- Add showDiff MCP tool for viewing git diffs in read-only diff tabs
- Add session-aware tab targeting — openFile/openDiff resolve workspace from session tab, insert after it
- Add merge conflict resolution: inline MergeView when file changes on disk while editing
- Add Cmd+Shift+R reload for editor tabs (images, PDFs, text)
- Show workspace status via border color on tab count badges (red/yellow/green)
- Fix selection coordinate offset caused by container padding
- Fix tab bar scroll jump when confirming tab rename with Enter
- Fix scroll events bubbling through archived tabs popup
- Resolve remote CWD fresh at drop time instead of caching at drag-enter
- Migrate old auto-resume commands on archived tabs at restore time

## v1.7.7

- Fix workspace suspend freezing view (infinite reactive loop from SvelteSet mutation in $effect.pre)
- Fix tabs being deleted on suspend instead of preserved for resume (pty-close guard during suspend)
- Improve "all suspended" empty state to distinguish single vs all workspaces suspended

## v1.7.6

- Add Rust-managed terminal selection with full scrollback support (drag-to-scroll, shift+click extend, double/triple-click word/line, Cmd+A select all)
- Fix white Preferences/Help window on Windows (WebView2 deadlock on sync command thread)
- Fix double-ssh tunnel commands in SSH MCP bridge

## v1.7.5

- Fix view not updating when suspending the active workspace (shows empty state with resume buttons)
- Scope drag-drop events to current window to prevent cross-window firing
- Refine SCP upload toast: clickable "list" action only for multi-file non-Claude SSH drops
- Move bolt indicator before auto-resume indicator in tab bar

## v1.7.4

- Add clickable toast actions (e.g. SCP upload toast opens uploaded files)
- Add native OS bell sound (macOS user-configured alert, Linux canberra, Windows SystemSounds)
- Add aitermTabId, aitermPort, aitermExport trigger variables
- Add scroll hold for scrollback (pause auto-scroll when viewing history)

## v1.7.3

- Fix duplicate event listeners by using window-scoped listen instead of global
- Filter out non-interactive SSH (git, scp) from bridge auto-detection

## v1.7.2

- Add file drop support for SSH terminals (SCP upload to remote CWD) and Claude sessions (upload to /tmp for file references)
- Add ~/.aiterm env file for tmux sessions with fallback sourcing on SessionStart
- Add reactive SSH bridge detection via title changes instead of one-shot timer
- Add "Install MCP for Current User" context menu for sudo/su scenarios
- Add "Inject aiTerm Env Vars" context menu for on-demand re-injection
- Recover Claude Code connection affinity on SSE reconnect from active sessions
- Add pending bridge state to prevent concurrent enableBridge race condition
- Fix white Preferences/Help windows on Windows (absolute asset paths)
- Remove obsolete Claude integration prompt modal

## v1.7.1

- Fix blank Preferences and Help windows on Windows (SvelteKit trailingSlash routing)
- Fix auto-resume command migration to catch additional old command patterns

## v1.7.0 — Performance overhaul for heavy workloads

- Move terminal backend to alacritty_terminal — all VTE parsing and buffer management in Rust, xterm.js as thin renderer (~60fps ANSI frames)
- Move scrollback persistence from JSON state to SQLite (WAL mode) — crash-safe, state file drops from ~25MB to ~32KB
- Fix critical UTF-8 corruption in scrollback restore (multi-byte chars split into C1 control sequences)
- Reduce scrollback memory pressure with dirty tracking and staggered saves
- Add lazy terminal tab activation — only spawn PTYs when tab becomes active
- Add workspace suspend/resume with auto-suspend timeout, sidebar controls, and context menus
- Add Claude Code hooks integration — replace trigger-based tracking with HTTP hooks (PreToolUse, PostToolUse, PreCompact, SessionStart/End, Stop, Notification)
- Add SSH MCP bridge — reverse tunnel for remote IDE tools with ControlMaster mux support and bridge status indicator
- Add Streamable HTTP MCP transport (POST /mcp), replacing legacy SSE
- Add per-monitor-count window geometry persistence with auto-repositioning on monitor changes
- Add remote file watching via SSH stat polling with host batching and backoff
- Add Claude session MCP tools (getClaudeSessions) for multi-agent coordination
- Add third-party license generation for Rust and npm dependencies
- Add UI font size preference with proportional rem-based scaling
- Improve notification system: sequential toast countdown, window focus awareness, dual toast + OS when unfocused
- Migrate auto-resume from triggers to hooks with old pattern detection and auto-migration
- Fix Preferences and Help windows not loading in production builds (missing .html extension)

## v1.6.2

- Preserve PTY when moving tabs between workspaces (drag to another workspace keeps the running session)
- Add multi-window MCP awareness with AITERM_TAB_ID env var and per-window event routing
- Add listWindows MCP tool and windowId parameter to listWorkspaces
- Graceful MCP server shutdown on app exit to release TCP port
- Improve import preview grouping for multi-window backups

## v1.6.1

- Add app diagnostics MCP tools (getDiagnostics, readLogs) with PTY stats, memory tracking, and trigger counters
- Add import preview modal with workspace selection, overwrite/merge modes, and gz backup support
- Improve backup import with deep merge, visual highlights for merged items, and ordering preservation
- Add PTY diagnostics and fix PTY leak on HMR remount
- Fix Cmd+Shift+R reloading wrong window's tab in multi-window
- Fix notes panel input reset by untracking local state in sync effects

## v1.6.0

- Add state backup/import with automatic daily backups and manual export
- Add editor file watching — detect external changes and prompt to reload
- Overhaul auto-resume: pin settings per tab, SSH session replay, edit menu, Cmd+Opt+R shortcut
- Add `replay_auto_resume` trigger action and context menu option

## v1.5.0

- Add tab-level scoping to triggers for per-tab pattern matching
- Expose preferences via MCP tools, rename Panels to Tabs in preferences UI
- Fall back to persisted auto-resume SSH when live PTY has no SSH on reload
- Clear trigger buffer when suppression window ends to prevent stale matches
- Sync PTY size on tab visibility, expand remote tilde paths

## v1.4.4

- Let CodeMirror handle all keyboard shortcuts when editor/diff tabs are active
- Add Editor section to help window with VS Code-style shortcuts
- Flatten help panel sections to use headings instead of accordions
- Keep tab bar visible when all tabs are closed

## v1.4.3

- Add findNotes MCP tool to search all tabs and workspaces for notes in one call
- Add auto-resume and trigger variable MCP tools (setTriggerVariable, getTriggerVariables, setAutoResume, getAutoResume)
- New tabs inherit the most common CWD/SSH setup from sibling tabs in the pane
- Add number-duplicated-tabs preference to control numeric prefix on duplicated tab names
- New workspaces insert after the active workspace instead of appending to end
- Fix TUI redraw dedup timestamp refresh to prevent false trigger re-fires

## v1.4.2

- Manage WebGL contexts per-terminal visibility lifecycle to stay within browser context limits
- Fix modifier tab buttons resizing without hover
- Extend auto-resume trigger suppression to 15s for SSH + Claude startup

## v1.4.1

- Add WebGL renderer for GPU-accelerated terminal rendering

## v1.4.0

- Add workspace, tab, and notes MCP tools with tab context discovery for Claude Code integration
- Add Cmd+/ passthrough to CodeMirror for toggle comment in editor tabs
- UI polish: tab button modes, workspace badges, IconButton fixes, delete confirmation
- Fix editor tab dirty indicator not clearing after save

## v1.3.4

- Convert Help from modal to standalone window with sidebar navigation
- Add About aiTerm dialog with credits and copyright
- Add Help menu with Report Bug and Feature Request links
- Add Preferences and Help buttons to sidebar footer

## v1.3.3

- Default file link click behavior to Cmd/Ctrl+Click, add Alt/Opt+Click option
- Fix auto-resume trigger overwriting custom commands; tab button now appends instead of replacing
- Fix invisible delete workspace button on hover
- Pin Linux CI to Ubuntu 22.04 for broader compatibility

## v1.3.2

- Fix Claude Code refusing to launch inside aiTerm ("cannot be launched inside another Claude Code session")

## v1.3.1

- Fix claude-resume trigger not matching session names that contain escaped quotes

## v1.3.0

- Add PDF viewer for editor tabs with page navigation
- Add markdown preview toggle for editor tabs with word wrap support
- Add file-type icons on editor/diff tabs (code, image, PDF, markdown)
- Add editor tab archive support with categorized dropdown (terminals, editors, diffs)
- Add editor tab reload and dirty indicator for unsaved changes
- Add OS notification deep-linking: clicking a notification navigates to the source tab
- Add file link click behavior preference (click, Cmd+click, or disabled)
- Add `COLORTERM=truecolor` to remote shell integration snippets
- Improve editor search match and selection visibility
- Fix editor horizontal scroll by constraining terminal-slot width
- Fix markdown relative image paths in preview mode
- Use `aiTermDev` as display name in dev builds for IDE integration

## v1.2.4

- Migrate existing auto-resume tabs to include SSH/CWD context on load
- Repair pre-interpolated auto-resume commands that contained stale variable values

## v1.2.3

- Fix auto-resume SSH context loss and show connection info in prompt

## v1.2.2

- Auto-update unmodified default triggers on app load when templates change
- Suppress trigger actions during post-mount scrollback restore
- Make file path detection always active with pre-compiled regex
- Restrict CI builds to version tags only

## v1.2.1

- Fix variable triggers not re-firing when captured values change
- Skip trigger variable cloning on shallow tab duplicates
- Persist OSC title as tab name so restarts show last-known title
- Include version in CI artifact names for Linux and Windows builds

## v1.2.0

- Add tab archiving: soft-close tabs with restore, sorted by recency with relative timestamps
- Add dynamic editor/diff themes based on active terminal theme
- Add Windows shell selection preference and prompt patterns
- Add auto-resume command migration for existing tabs
- New tabs open at the most common CWD among workspace tabs
- Switch to newly duplicated tab after clone
- Extract reusable IconButton, Button, and StatusDot components
- Add themed tooltip support to StatusDot and IDE Connected indicator
- Add copy button and text selection to editor error messages
- Adapt logo brightness for light themes
- Fix Solarized Light theme colors
- Fix DiffPane scroll/layout, viewport locking, and trigger dedup
- Fix legacy language modes not loading in production builds
- Fix Windows PTY lag, hang on quit, multi-window freeze, and close button
- Fix Linux process introspection: use `/proc` for CWD, correct `ps` flags
- Isolate dev/production MCP server registration in `~/.claude.json`
- Preserve original tab name through archive/restore cycle

## v1.1.0

- Add Claude Code IDE integration: WebSocket server for open-file/open-diff commands, connected status in sidebar
- Add diff editor tab using CodeMirror merge view
- Add Linux and Windows bundling support with platform guards
- Add GitHub Actions CI workflow for cross-platform builds
- Add NSIS installer config for Windows
- Add workspace `default_command` preference
- Default to PowerShell on Windows, skip shell integration hooks
- Gate Unix-specific PTY code (`lsof`, `ps`, shell hooks) with `#[cfg(unix)]`
- Gate macOS-specific window APIs (hidden title, title bar style) to macOS only
- Add editor registry for cross-component editor instance access

## v1.0.0

- Add CodeMirror 6 editor tabs: open files from terminal output or via `Cmd+O`, syntax highlighting for 30+ languages
- Add image preview in editor tabs with zoom controls for local and remote files
- Add OSC 8 file hyperlinks: `l` shell function emits clickable file links in terminal
- Add variable-match triggers with condition expressions (`&&`, `||`, `!`, `==`, `!=`)
- Add `enable_auto_resume` trigger action for automatic Claude Code auto-resume
- Add Claude Code integration modal with default triggers for session management
- Add workspace-level notes alongside tab-level notes
- Add workspace sidebar preferences: sort order, tab count display, recent workspaces toggle
- Add notification sounds for trigger alerts
- Add deeper OSC integration and tab state indicators
- Remove prompt indicator from tabs; gate completion indicator on minimum duration
- Close tab now selects previous (left) tab instead of next
- Editor tabs support split pane via `Cmd+D`
- File path link provider only active while `Cmd/Ctrl` held
- Strip orphaned SGR 4 underline from serialized scrollback
- `Cmd+O` file dialog defaults to active terminal CWD

## v0.9.0

- Add trigger system: watch terminal output for regex patterns, fire actions (notify, send command)
- Add trigger variables: capture groups extracted into named variables with `%varName` interpolation
- Add default triggers for Claude Code (`claude-resume`, `claude-session-id`)
- Overhaul notification system: three modes (auto, in-app, native, disabled) with in-app toast UI
- Add reusable Toggle, Select, and InlineConfirm components
- Add trigger management UI in Preferences
- Fix tab rename incorrectly setting `custom_name` when exiting edit mode without changes

## v0.8.3

- Redesign tab styling: full border for active tab, colored underline for activity indicators

## v0.8.2

- Persist notes panel open/closed state per tab across sessions
- Fix titlebar window dragging when notes panel is open

## v0.8.1

- Add centered workspace name to macOS title bar
- Improve notes panel: interactive checkboxes in rendered mode, better default styling and contrast

## v0.8.0

- Add notes panel per tab with source/rendered mode toggle
- Add notes preferences (font size, font family, width, word wrap)
- Add `Cmd+Shift+N` keyboard shortcut to toggle notes panel
- Show indicator dot on tabs with notes content

## v0.7.1

- Add macOS menu items for Preferences, Reload All Windows, and Reload Current Window
- Add recent workspaces section to sidebar
- Add `%title` support for tab names via clickable URLs
- Ignore small PTY writes for tab activity detection

## v0.7.0

- Add auto-resume support for local (non-SSH) terminals
- Rename internal "pin" terminology to "auto-resume" (backward-compatible)
- Add `Cmd+R` keyboard shortcut to toggle auto-resume
- Add auto-resume command prompt as textarea with autogrow and manual resize
- Persist remembered auto-resume command across enable/disable cycles
- Add `Cmd+click` on duplicate tab button to skip scrollback
- Replace duplicate tab SVG icon with Unicode character
- Add changelog modal (click version number in sidebar)

## v0.6.0

- Fix SSH `ControlMaster auto` causing "socket already exists" warnings on restore
- Add tab rename UX improvements (double-click to rename, clear to reset)
- Add Tauri MCP bridge for dev automation (feature-gated, excluded from production)

## v0.5.0

- Internal release (no user-facing changes)

## v0.4.0

- Add OSC 133 shell integration for command completion detection
- Add tab indicators: completed (checkmark/cross), prompt, activity dot
- Add preferences window with shell integration settings
- Add remote shell integration install command (permanent, writes to rc file)
- Remove running spinner (unreliable with interactive programs like SSH, vim)
- Fix remote OSC 133 sequence handling

## v0.3.1

- Add workspace activity indicator in sidebar
- Fix terminals killed on workspace switch (lazy activation pattern)
- Fix terminal re-attachment after split tree changes
- Fix alternate screen artifacts in restored scrollback
- Add DMG icon stamping and limit bundle to DMG-only

## v0.3.0

- Add multi-window support with independent workspaces per window
- Add session restore (persist and restore terminal state across app restarts)
- Add structured logging with tauri-plugin-log
- Isolate dev/production data directories
- Add drag tab to workspace and custom theme editor
- Add built-in theme system with 10 themes
- Add sidebar collapse
- Add tab drag/drop reordering and shell title integration
- Add configurable duplication preferences for split pane cloning
- Add OSC 7 support for accurate cwd detection on split
- Add custom prompt patterns for remote cwd detection
- Add iTerm2-style recursive split panes with context cloning
- Add file drag-drop and clipboard file paste
- Add find-in-terminal (Cmd+F) and font zoom (Cmd+/-)
- Add right-click context menu with iTerm2-style Cmd+C/V
- Add background tab activity indicator
- Add app icon, titlebar logo, loading screen, and favicon
- Fix data-loss bugs, resource leaks, and security issues

## v0.1.0

- Initial release: Tauri-based terminal emulator with workspace organization
- Workspaces, panes, tabs
- xterm.js terminal with fit, serialize, and web-links addons
- Scrollback persistence
