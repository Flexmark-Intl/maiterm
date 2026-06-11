# maiTerm

A Tauri-based terminal emulator with workspace organization, built with Svelte 5 and Rust.

## Tech Stack

- **Frontend**: Svelte 5 (runes), SvelteKit, TypeScript, Vite
- **Backend**: Rust, Tauri 2
- **Terminal**: alacritty_terminal (Rust VTE parser + buffer) with xterm.js as thin renderer (scrollback=0; DOM renderer by default — canvas/webgl ghost under full-frame streaming; fit, web-links addons)
- **Editor**: CodeMirror 6 (+ MergeView for diffs)
- **PTY**: portable-pty for cross-platform pseudo-terminal support
- **State**: parking_lot RwLock for thread-safe Rust state

## Project Structure

```
src/                          # Frontend (Svelte/TypeScript)
├── routes/                   # SvelteKit routes
│   ├── +layout.svelte        # App shell, keyboard shortcuts, modals
│   └── +page.svelte          # Main terminal view, portal rendering
├── lib/
│   ├── components/           # Svelte components
│   │   ├── editor/           # EditorPane (CodeMirror 6), DiffPane (MergeView)
│   │   ├── terminal/         # TerminalPane, TerminalTabs
│   │   ├── workspace/        # WorkspaceSidebar
│   │   └── pane/             # SplitPane
│   ├── stores/               # Svelte 5 stores (.svelte.ts)
│   │   ├── workspaces.svelte.ts   # Workspace/pane/tab CRUD, navigateToTab()
│   │   ├── terminals.svelte.ts    # Terminal instances, OSC state
│   │   ├── preferences.svelte.ts  # User preferences
│   │   ├── activity.svelte.ts     # Tab activity indicators (OSC 133)
│   │   ├── triggers.svelte.ts     # Trigger engine (pattern matching, variables)
│   │   ├── claudeCode.svelte.ts   # Claude Code IDE tool request handler
│   │   ├── claudeState.svelte.ts  # Claude session state from hooks (active/idle/permission)
│   │   ├── sshMcpBridge.svelte.ts # SSH MCP bridge orchestration, reactive status
│   │   ├── editorRegistry.svelte.ts # Editor state tracking (dirty, view refs)
│   │   ├── notifications.svelte.ts  # Command completion notification logic
│   │   ├── toasts.svelte.ts       # In-app toast notification store
│   │   └── notificationDispatch.ts # Routes to toast or OS notification
│   ├── triggers/             # Trigger definitions and parsing
│   ├── themes/               # Theme system
│   ├── utils/                # Pure utility modules
│   └── tauri/                # Tauri IPC layer (commands.ts, types.ts)

src-tauri/src/                # Backend (Rust)
├── lib.rs                    # Tauri app setup, command registration
├── commands/                 # Tauri command handlers
├── claude_code/              # Claude Code IDE integration (MCP server)
├── terminal/                 # Terminal backend (alacritty_terminal)
│   ├── handle.rs             # TerminalHandle, TermDimensions, create_terminal()
│   ├── event_proxy.rs        # AitermEventProxy (EventListener → Tauri events)
│   ├── render.rs             # Grid → ANSI viewport renderer (~60fps)
│   ├── osc.rs                # OscInterceptor (OSC 7/9/133/633/1337)
│   ├── search.rs             # Buffer search via RegexSearch
│   └── serialize.rs          # Buffer serialization/restore via VTE parser
├── state/                    # Application state + persistence
└── pty/                      # PTY management
```

**Module-specific docs**: Detailed documentation for individual subsystems lives in CLAUDE.md files within their directories:
- `src/lib/components/terminal/CLAUDE.md` — Portal pattern, terminal architecture (alacritty_terminal + xterm.js), OSC, shell integration, split cloning
- `src/lib/components/editor/CLAUDE.md` — CodeMirror, diff tabs, editor registry
- `src-tauri/src/claude_code/CLAUDE.md` — Claude Code IDE integration, SSH MCP bridge
- `src/lib/triggers/CLAUDE.md` — Trigger engine, defaults, variables, dedup

## Commands

```bash
npm run dev          # Start Vite dev server (frontend only)
npm run check        # TypeScript + Svelte type checking
npm run tauri:dev    # Full app development (frontend + backend + MCP bridge)
npm run tauri:build  # Production build
cargo check          # Check Rust compilation (in src-tauri/)
```

**Note**: `npm run tauri:dev` passes `--features mcp-bridge --config src-tauri/tauri.dev.conf.json` to enable the Claude Code MCP bridge and apply dev-specific CSP overrides.

## Key Patterns

### Svelte 5 Stores

Stores use the runes API with a factory function pattern:

```typescript
function createMyStore() {
  let value = $state<Type>(initial);

  return {
    get value() { return value; },  // Getter for reactivity

    async setValue(newValue: Type) {
      await commands.setValue(newValue);  // Persist to backend
      value = newValue;                   // Update local state
    }
  };
}

export const myStore = createMyStore();
```

### Tauri Commands

1. Define Rust struct in `state/workspace.rs` with serde derives
2. Add command in `commands/workspace.rs`
3. Register in `lib.rs` invoke_handler
4. Add TypeScript type in `tauri/types.ts`
5. Add wrapper function in `tauri/commands.ts`

### Component Patterns

- **Modals**: Follow `HelpModal.svelte` pattern (backdrop, escape key, close button)
- **Reactive effects**: Use `$effect()` for side effects, return cleanup function if needed
- **Props**: Use `$props()` with TypeScript interface

### Rust State

- All state wrapped in `Arc<AppState>` and managed by Tauri
- Use `state.app_data.read()` for queries, `state.app_data.write()` for mutations
- Always call `save_state(&app_data)` after mutations

## Styling

**Theme system**: 10 built-in themes + custom theme support. Default is Tokyo Night.

```css
--bg-dark: #1a1b26;     /* Main background */
--bg-medium: #24283b;   /* Elevated surfaces */
--bg-light: #414868;    /* Borders, hover states */
--fg: #c0caf5;          /* Primary text */
--fg-dim: #565f89;      /* Secondary text */
--accent: #7aa2f7;      /* Interactive elements */
```

Themes defined in `src/lib/themes/index.ts`. Applied via `applyUiTheme()` which sets CSS variables on `document.documentElement`. Use CSS variables from `app.css`. Component styles are scoped.

## Data Model

```
Workspace
├── id, name
├── panes: Pane[]
├── active_pane_id
├── split_root: SplitNode (binary tree of pane layout)
└── notes: WorkspaceNote[] (workspace-level notes)

Pane
├── id, name
├── tabs: Tab[]
└── active_tab_id

Tab
├── id, name, custom_name (bool — true if user explicitly renamed)
├── tab_type: 'terminal' | 'editor' | 'diff'
├── pty_id (terminal tabs — links to running PTY)
├── editor_file (editor tabs — EditorFileInfo)
├── diff_context (diff tabs — DiffContext)
├── scrollback (serialized terminal state)
├── notes, notes_open, notes_mode (per-tab markdown notes)
└── trigger_variables (persisted variable map from triggers)

SplitNode = SplitLeaf { pane_id } | SplitBranch { id, direction, ratio, children }

Preferences
├── theme, custom_themes, font_size, font_family
├── cursor_style, cursor_blink
├── auto_save_interval, scrollback_limit
├── prompt_patterns, notification_mode, notification_sound
├── clone_cwd, clone_scrollback, clone_ssh, clone_history, clone_notes
├── claude_code_ide, claude_code_ide_ssh
├── triggers, hidden_default_triggers
└── (see state/workspace.rs for full list)
```

## Notifications

Three-mode notification system controlled by `notification_mode` preference:

- **auto** (default): In-app toasts when window is focused, OS notifications when unfocused
- **in_app**: Always show in-app toasts
- **native**: Always use OS notifications
- **disabled**: No notifications

Architecture: `notificationDispatch.ts` routes `dispatch(title, body, type, source?)` calls based on mode + focus state. Toast UI in `Toast.svelte` (rendered in `+layout.svelte`), store in `toasts.svelte.ts` (max 3 visible, configurable auto-dismiss).

**Deep-linking**: Both toasts and OS notifications carry `source.tabId`. Clicking a toast calls `navigateToTab(tabId)`. **Note**: `onAction` only fires on mobile (iOS/Android) — desktop `notify_rust` is fire-and-forget.

**Command completion** (`notifications.svelte.ts`): Only notifies if tab is not visible and command duration exceeds `notify_min_duration` (default 30s).

## Dev/Production Isolation

Dev and production builds use **separate data directories** so they can run simultaneously without state corruption:

- **Dev** (`tauri:dev`): `~/Library/Application Support/com.aiterm.dev/`
- **Production** (`tauri:build`): `~/Library/Application Support/com.aiterm.app/`

Controlled by `cfg!(debug_assertions)` in `state/persistence.rs` → `app_data_slug()`. **Do not** hardcode `com.aiterm.app` anywhere — always use `app_data_slug()` in Rust.

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| Cmd+T | New tab |
| Cmd+W | Close tab (or pane if last tab) — requires two presses within 2s |
| Cmd+1-9 | Switch to tab |
| Cmd+Shift+[ / ] | Previous / next tab |
| Cmd+Shift+T | Duplicate tab |
| Cmd+Shift+R | Reload tab (duplicate + close original) |
| Cmd+D | Split pane (duplicate current tab) |
| Cmd+N | New window |
| Cmd+Shift+N | Duplicate window |
| Cmd+O | Open file in editor tab |
| Cmd+S | Save file (editor tabs only) |
| Cmd+F | Find/replace (editor tabs) / terminal search |
| Cmd+G | Goto line (editor tabs; find-next while search panel is open) |
| Cmd+E | Toggle notes panel |
| Cmd+Shift+C | Toggle composer dock |
| Cmd+, | Preferences |
| Cmd+/ | Help |

Defined in `+layout.svelte` handleKeydown. Cmd+F/K/S/D intercepted in capture phase — returns early for editor tabs to let CodeMirror handle them.

## Type Safety

- Rust structs and TypeScript interfaces must stay in sync
- Use `snake_case` for Rust/serde, same in TypeScript (not camelCase)
- Tauri commands return `Result<T, String>` for error handling

## Adding a New Feature Checklist

### New Tauri Command
1. [ ] Add/modify struct in `src-tauri/src/state/workspace.rs`
2. [ ] Add command function in `src-tauri/src/commands/workspace.rs`
3. [ ] Register command in `src-tauri/src/lib.rs` generate_handler!
4. [ ] Export types from `src-tauri/src/state/mod.rs` if new
5. [ ] Add TypeScript interface in `src/lib/tauri/types.ts`
6. [ ] Add invoke wrapper in `src/lib/tauri/commands.ts`
7. [ ] Run `cargo check` and `npm run check`

### New Store
1. [ ] Create `src/lib/stores/mystore.svelte.ts`
2. [ ] Use factory function pattern with `$state` runes
3. [ ] Export getters for reactive access
4. [ ] Call Tauri commands in async methods

### New Modal
1. [ ] Create component following `HelpModal.svelte` pattern
2. [ ] Add state variable in `+layout.svelte`
3. [ ] Add keyboard shortcut in handleKeydown
4. [ ] Render modal at bottom of `+layout.svelte`

## Debugging

### Logging

Uses `tauri-plugin-log` — all logs go to a log file, stdout, and (in dev) browser devtools via `attachConsole()`. Rust and frontend share the same log file.

**Rust** — use `log` crate macros (`log::info!`, `log::warn!`, `log::error!`, `log::debug!`).
**Frontend** — import from `@tauri-apps/plugin-log` (`error`, `info`, `warn`, `debug`).
Do **not** use `eprintln!()` or `console.error()` — they bypass the log file.

Uncaught webview errors and unhandled promise rejections are captured in `+layout.svelte` and forwarded to the log tagged `[WEBVIEW_ERROR]` — `grep "\[WEBVIEW_ERROR\]" aiterm.log` to find renderer-side exceptions that may precede a WebContent crash.

### Log file locations

| OS | Dev | Prod |
|----|-----|------|
| **macOS** | `~/Library/Logs/com.aiterm.app/aiterm-dev.log` | `~/Library/Logs/com.aiterm.app/aiterm.log` |
| **Linux** | `~/.config/aiterm/logs/aiterm-dev.log` | `~/.config/aiterm/logs/aiterm.log` |
| **Windows** | `%APPDATA%\aiterm\logs\aiterm-dev.log` | `%APPDATA%\aiterm\logs\aiterm.log` |

### State file

Check `~/Library/Application Support/com.aiterm.dev/aiterm-state.json` (dev) or `com.aiterm.app/` (prod)

### Post-crash forensics

The same data dir holds `aiterm-running.marker`, refreshed each minute by the memory sampler and deleted by `exit_app`. If it survives to the next launch, the previous run died uncleanly — surfaced as `previous_run.crashed` + `previous_run.marker_mtime_secs` (upper bound on time-of-death) in `getDiagnostics`.

Also in `getDiagnostics`: `crash_reports` — newest 20 entries from `~/Library/Logs/DiagnosticReports/` (and `Retired/`) within the last 30 days, filtered to `maiTerm*` and `com.apple.WebKit.WebContent*` `.ips`/`.crash` files. Each entry has `mtime_secs`, `process`, `exception_type`, `termination_reason`. May require Full Disk Access on macOS to read; silently empty on permission failure (`check_full_disk_access` is wired into the UI).

Memory trend (`aiterm-memory-trend.json`) is reseeded into the in-memory ring buffer at startup so RSS history survives a restart — see the Logging note about `[WEBVIEW_ERROR]` for the JS-side complement.

## Common Pitfalls

- **Async in onMount**: Don't make onMount async, use IIFE or fire-and-forget instead
- **Effect cleanup**: Return cleanup function from `$effect()` when setting up intervals/listeners
- **Map reactivity**: When mutating Maps in stores, create new Map: `instances = new Map(instances)`
- **PTY lifecycle**: Kill PTY in onDestroy, save scrollback before disposal
- **`\u` in Svelte templates**: Interpreted as unicode escape. Use expression syntax: `{'\\u'}`
- **Svelte $effect reactive loops with stores**: `clearFoo()` that reads `$state` inside `$effect` subscribes the effect to that state. Wrap in `untrack()` to prevent re-triggering.
- **`confirm()` doesn't work in Tauri webviews**: Use inline confirmation UI instead of `window.confirm()`.
- **Tauri PluginListener cleanup**: `onAction()` and similar return `PluginListener` objects, not functions. Clean up with `.unregister()`.
- **Serde round-trip pitfall**: Rust `skip_serializing_if = "Option::is_none"` omits null fields → loaded JS objects have `undefined` instead of `null`. Use field-by-field comparison with `?? null` normalization, NOT `JSON.stringify`.
- **New workspace insert order**: New workspaces insert after the currently active workspace (not appended to end), persisted via `reorderWorkspaces`.
- **Width resizes mid-stream duplicate TUI scrollback**: Claude Code re-renders its retained transcript on every cols change; the old rendering stays in history → permanent duplicate blocks. `resize_pty` coalesces resizes (250ms trailing debounce while output is hot, no-ops skipped), and background tabs spawn at their saved size instead of 80×24 to avoid a width jump on first view. Never add code paths that fire gratuitous width changes at a streaming PTY.
