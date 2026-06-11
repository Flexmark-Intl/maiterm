# Terminal Components

## Architecture: alacritty_terminal + xterm.js

Terminal parsing and buffer management runs in Rust via `alacritty_terminal`. xterm.js serves only as a thin renderer (scrollback=0, ~2KB per terminal).

```
PTY reader thread (Rust)
  → raw bytes to OscInterceptor (extracts OSC 7/133/9/1337, emits Tauri events)
  → raw bytes to alacritty_terminal::Term (VTE parse + buffer management)
  → Term's EventListener emits Event::Title, Event::Bell, Event::ClipboardStore → Tauri events
  → render_viewport() extracts visible cells → ANSI string (throttled ~60fps)
  → emit "term-frame-{ptyId}" to frontend (xterm.js writes ANSI for rendering)
  → emit "pty-raw-{ptyId}" for frontend trigger engine (temporary bridge)

Frontend (xterm.js scrollback=0):
  → receives ANSI frame via term-frame event, writes to terminal for rendering only
  → listens to term-title, term-osc7, term-osc133, term-bell, term-clipboard events
  → routes to existing stores (terminalsStore, activityStore, triggers)
```

**Key Rust files** (`src-tauri/src/terminal/`):
- `handle.rs` — `TerminalHandle` wraps `Term<AitermEventProxy>`, `OscInterceptor`, VTE `Processor`
- `event_proxy.rs` — `AitermEventProxy` implements `EventListener`, routes Title/Bell/Clipboard/PtyWrite events
- `render.rs` — `render_viewport()` iterates grid cells, emits SGR sequences, returns `TerminalFrame`
- `osc.rs` — `OscInterceptor` state machine scans raw bytes for OSC 7/9/133/633/1337
- `search.rs` — buffer search using `RegexSearch` (replaces @xterm/addon-search)
- `serialize.rs` — buffer serialization/restore via VTE parser (replaces @xterm/addon-serialize)

**Tauri commands** (7 new):
- `scroll_terminal`, `scroll_terminal_to`, `get_terminal_scrollback_info`
- `search_terminal`, `serialize_terminal`, `restore_terminal_scrollback`, `resize_terminal_grid`

**Frontend event listeners** (TerminalPane.svelte):
- `term-frame-{ptyId}` — rendered ANSI viewport from Rust → `terminal.write(frame.ansi)`
- `pty-raw-{ptyId}` — raw bytes for trigger engine + activity tracking
- `term-title-{ptyId}` — title changes (OSC 0/2)
- `term-osc7-{ptyId}` — CWD reports (OSC 7, iTerm2 1337)
- `term-osc133-{ptyId}` — shell integration (OSC 133/633)
- `term-notification-{ptyId}` — notification requests (OSC 9)
- `term-clipboard-{ptyId}` — clipboard set (OSC 52)
- `term-bell-{ptyId}` — terminal bell

**Resize coalescing (`pty/manager.rs::resize_pty`)**: TUIs (Claude Code/Ink) re-render their retained transcript on every *width* change; mid-stream, the previous rendering has already scrolled into history where it can't be erased, so each SIGWINCH leaves a permanent duplicate block in scrollback (rows-only changes don't re-wrap and are harmless). `resize_pty` therefore: (1) skips resizes that match the current grid; (2) while output is hot (last PTY read < 1s — an active TUI's spinner keeps this true), defers the resize with a 250ms trailing debounce and applies only the final size — an A→B→A flap nets zero SIGWINCHes. Idle resizes apply immediately. PTY and alacritty grid are always resized together.

**Background tab sizing**: hidden tabs can't be measured at mount, so they'd spawn at xterm's 80×24 default and take a width jump on first view (→ TUI re-render → scrollback duplication). `save_terminal_scrollback` records the grid size (cols/rows columns in the SQLite scrollback DB); a hidden fresh spawn applies `getSavedTerminalSize(tabId)` before spawning, so the later fit is a no-op when the restored layout matches.

**Scrollback lifecycle**:
- Serialization: `serializeTerminal(ptyId)` calls Rust to serialize full buffer as ANSI string
- Restore: `restoreTerminalScrollback(ptyId, scrollback)` feeds ANSI through VTE parser into Term
- Auto-save: periodic `serializeTerminal` → `setTabScrollback` (staggered, dirty-tracked)
- Old xterm.js SerializeAddon format is backward compatible (both produce ANSI)
- Rust `serialize_terminal` returns error when alternate screen is active (skips TUI content)
- Orphaned SGR 4 underlines from old OSC 8 links are stripped in Rust's `strip_orphaned_underlines()`

## Portal Pattern (Terminal Persistence)

When the split tree changes (leaf → split node), Svelte destroys and recreates the entire subtree. To prevent terminals from being killed and recreated:

- **TerminalPanes render flat** at the `+page.svelte` level in a keyed `{#each}` block over all tabs
- **SplitPane renders empty slot divs** with `data-terminal-slot={tab.id}`
- **TerminalPane portals** its `containerRef` into the matching slot via `attachToSlot()`
- **SplitPane dispatches** `terminal-slot-ready` CustomEvents on mount so TerminalPanes can re-attach after splits
- Guard `fitWithPadding` with `containerRef.isConnected` to skip when detached between portal moves

**Do not** move TerminalPane rendering into SplitPane — this breaks terminal persistence on split.

**EditorPane and DiffPane use the same portal pattern** — `attachToSlot()` portals into `data-terminal-slot={tabId}`, listens for `terminal-slot-ready`.

## Tab Move Between Workspaces (PTY Preservation)

Dragging a terminal tab to another workspace preserves the running PTY instead of killing and respawning:

- **`terminalsStore.preservePty(ptyId)`** — called before the move, prevents `onDestroy` from killing the PTY
- **`terminalsStore.consumePreserve(ptyId)`** — consumed once at the top of `onDestroy`; when set, skips `killTerminal` **and** skips `disableBridge`. The SSH MCP bridge is keyed by `tabId` (unchanged across a move), so the reattaching pane keeps using it. Tearing it down on move would drop the bridge state, and the new pane's `term-title` handler would then re-run `enableBridge`, re-injecting `export AITERM_TAB_ID=…` into the live (e.g. compacting) session.
- **Backend `move_tab_to_workspace`** — atomically moves the tab (with `pty_id`) between workspaces
- **`existingPtyId` prop** — `+page.svelte` passes `tab.pty_id` only when `terminalsStore.get(tab.id)` is truthy (avoids reattach on app restart with stale PTY IDs)
- **New TerminalPane reattach** — when `existingPtyId` is set, skips `spawnTerminal`, SSH replay, and auto-resume; sets up fresh event listeners and registers with the store

**Reattach sizing**: the new xterm instance is synced to the live alacritty grid size (`getTerminalScrollbackInfo().viewport_cols/rows`) before the refit, so the running TUI never sees an 80×24 transient. The post-layout `resizeTerminal` is a no-op in Rust when the fitted size matches the grid — an unchanged layout sends no SIGWINCH at all.

## xterm.js Notes

- Terminal created with `new Terminal({ scrollback: 0, ... })` — Rust manages all scrollback
- Required addons: FitAddon (resize), WebLinksAddon (clickable links), CanvasAddon (renderer)
- **No SerializeAddon or SearchAddon** — these are replaced by Rust commands
- **Renderer (default: DOM)**: xterm's built-in DOM renderer is the default and is correct here — no GPU/canvas backbuffer, so it can't ghost. Controlled by the `terminal_renderer` preference (`"dom"` | `"canvas"`, Preferences → Terminal → Rendering). **Both** `@xterm/addon-webgl` and `@xterm/addon-canvas` ghost under our workload: Rust streams a full-viewport repaint (`\x1b[H\x1b[2J` + content) ~60fps, and their alpha-blended / partial-clear backbuffers don't fully overwrite the previous frame, so diff backgrounds and rapid input redraws smear across rows until enough repaints accumulate (the red-stripe artifact and staircased-input artifact). WebGL was dropped first; Canvas was the next attempt and ghosts too — so DOM became the default. Canvas stays available as an opt-in for side-by-side comparison: loaded only on the visible tab, disposed when hidden or when the pref is `"dom"` (disposal reverts to the DOM renderer + a `terminal.refresh()`). maiTerm renders only one bounded viewport (`scrollback:0`), so the canvas/webgl throughput advantage never applied anyway.
- Call `fitAddon.fit()` after container resize or font changes
- Options can be updated at runtime via `terminal.options.propertyName`

## OSC 8 File Hyperlinks (`l` Command)

The `l` shell function wraps `ls -la` and emits OSC 8 hyperlinks (`file://hostname/path`) for each file, making filenames clickable in the terminal.

**Injection**: Always injected via `PROMPT_COMMAND` (bash) or ZDOTDIR shim (zsh) in `pty/manager.rs`, regardless of shell integration preference. Also available in remote shells via `shellIntegration.ts`.

**Link handling**: `TerminalPane.svelte` registers a `linkHandler` for `file://` URIs. On activate, calls `openFile()` from `openFile.ts`. Context menu adds "Copy Full Path" for hovered file links.

**Underline behavior**: xterm.js hardcodes `UnderlineStyle.DASHED` for any cell with a `urlId`. We override with `.xterm-underline-5 { text-decoration: none; }`.

**Scrollback cleanup**: Orphaned SGR 4 underlines from old OSC 8 links are stripped in Rust's `strip_orphaned_underlines()` during scrollback restore.

**File path detection**: `filePathDetector.ts` implements xterm's `ILinkProvider`. Only active when Cmd/Ctrl is held.

## OSC State

`terminals.svelte.ts` manages per-terminal OSC state (title, cwd, cwdHost). OSC events are now emitted from Rust:

- **OSC 0/2** (title): Emitted via `AitermEventProxy` → `term-title-{ptyId}`
- **OSC 7** (cwd): Parsed by `OscInterceptor` → `term-osc7-{ptyId}`
- **OSC 133/633** (shell integration): Parsed by `OscInterceptor` → `term-osc133-{ptyId}`
- **OSC 9** (notification): Parsed by `OscInterceptor` → `term-notification-{ptyId}`
- **OSC 52** (clipboard): Handled by `AitermEventProxy` → `term-clipboard-{ptyId}`
- **Listener API**: `onOscChange(fn)` for reactive subscriptions (used by TerminalTabs)

## Shell Integration

OSC 133 (FinalTerm protocol) detects command start/finish for tab indicators. Controlled by `shell_integration` preference.

**Protocol**: `A` = prompt start, `B` = command start, `D;exitcode` = command finished

**Local hooks** (Rust `pty/manager.rs`): Injected via env vars / ZDOTDIR shim before the shell starts.

**Remote hooks** (`src/lib/utils/shellIntegration.ts`): Two context menu modes:
- **Setup Shell Integration** — sends a one-liner to the current session (temporary). Uses `buildShellIntegrationSnippet()`.
- **Install Shell Integration** — writes to `~/.bashrc` or `~/.zshrc` via heredoc (permanent). Uses `buildInstallSnippet()`.

**Tab indicators** (`activity.svelte.ts` + `claudeState.svelte.ts`): Priority: alert (❗) > question > Claude state (pulsing accent = active, green dot = idle, lock = permission) > shell state (completed/prompt/activity dot). Shell state only shown on inactive tabs. Claude state indicators rendered in `TerminalTabs.svelte`.

**OSC 133 A + SSH MCP bridge**: The prompt-start handler (`cmd === 'A'`) checks `getPtyInfo()` before tearing down the SSH MCP bridge. Remote shells emit OSC 133 A on every prompt, which would falsely disable the bridge. The guard ensures the bridge is only torn down when the local shell is at a prompt (no foreground SSH command).

## Split Cloning (Pane Duplication)

`splitPaneWithContext()` in `workspaces.svelte.ts` handles pane duplication:

1. Serializes scrollback via `serializeTerminal()` (Rust command)
2. Gets PTY info via `getPtyInfo()` — returns local cwd (via lsof) and foreground SSH command
3. Creates new pane with scrollback pre-populated
4. Copies shell history (`copyTabHistory`)
5. Stores split context for the new TerminalPane to consume on mount

### SSH Session Cloning

When source has active SSH, `buildSshCommand()` constructs:
```
ssh -t user@host 'cd ~/path && exec $SHELL -l'
```

### Remote CWD Detection

Priority: OSC 7 (if not stale) → prompt pattern heuristic.

**Stale OSC 7 detection**: Compare OSC 7 cwd with lsof-reported local cwd. If equal, OSC 7 is stale.

**Prompt patterns**: User-configurable in Preferences > Shell. See `src/lib/utils/promptPattern.ts`.

### Shell Escaping

`shellEscapePath()` handles quoting for remote shells:
- `~` left unquoted for expansion, rest single-quoted: `~/path` → `~/'path'`
- Single quotes in paths escaped as `'\''`

## New Tab Context Inheritance

When creating a new tab (Cmd+T / + button), the workspace's dominant CWD or SSH setup is inherited:

1. Queries live PTY info for all terminal tabs in the active pane
2. Counts occurrences — the most common setup wins
3. SSH setups inherit both the SSH command and remote CWD; local setups inherit just the CWD

## Terminal-Specific Pitfalls

- **TUI redraws cause false triggers and activity**: Detect redraws via `\e[A`, `\e[H`, `\e[J` in raw PTY data. In triggers: replace buffer instead of appending. In activity: skip `markActive()`.
- **OSC 133 replayed from scrollback**: Gate the OSC 133 handler on `trackActivity` flag (delayed 2s after mount) to ignore stale sequences.
- **Renderer ghosting (why DOM is default)**: GPU-backed renderers leave stale cells under our full-frame streaming. `@xterm/addon-webgl`'s alpha-blended backbuffer ghosts glyphs on redrawn styled/animated cells; `@xterm/addon-canvas` ghosts too — it leaves diff-line red backgrounds smeared across unrelated rows and staircases in-progress input while typing during heavy output (clears only after sustained repaints). The grid in Rust is correct in all these cases (`render.rs` emits `\x1b[H\x1b[2J` + full content every frame) — the artifact is renderer-only. Default to the DOM renderer (`terminal_renderer` pref); only load the Canvas addon when explicitly selected, and only on the visible tab.
- **Hover state cleared before context menu interaction**: Use a plain (non-reactive) variable for the snapshot, set it at open time.
- **Single quotes prevent ~ expansion**: `cd '~/path'` fails on remote. Use `cd ~/'path'` instead.
- **Shell escaping layers**: JS → local shell → SSH → remote shell. `$SHELL` must not be escaped.
- **PROMPT_COMMAND guard flag must be last**: `__aiterm_at_prompt=1` MUST be the final item in PROMPT_COMMAND.
- **SSH ControlMaster on restore**: `buildSshCommand()` injects `-o ControlMaster=no`, and `cleanSshCommand()` strips it to prevent flag accumulation.
