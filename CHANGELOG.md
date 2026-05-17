# Changelog

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
