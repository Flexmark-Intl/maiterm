# Editor Components

## CodeMirror Editor Tabs

Editor tabs (`tab_type === 'editor'`) render `EditorPane.svelte` instead of `TerminalPane.svelte`. They exist alongside terminal tabs in the same pane.

**Key files**:
- `src/lib/components/editor/EditorPane.svelte` — main component
- `src/lib/utils/editorTheme.ts` — Tokyo Night CM6 theme (matches terminal colors)
- `src/lib/utils/languageDetect.ts` — language detection + dynamic CM6 language loader
- `src/lib/utils/openFile.ts` — orchestrates open flow (local vs remote, fetch, tab creation)
- `src-tauri/src/commands/editor.rs` — `read_file`, `write_file`, `read_file_base64`, `scp_read_file`, `scp_write_file`, `scp_read_file_base64`, `create_editor_tab`

**Language loading**: `loadLanguageExtension(langId)` dynamically imports the CM6 language package. First-class packages (js, ts, python, rust, html, css, json, etc.) are preferred; legacy `StreamLanguage` modes cover 30+ additional languages. Detection priority: explicit `editorFile.language` → shebang → file extension → filename.

**Image preview**: `isImageFile()` checks extension; if true, loads via `read_file_base64` / `scp_read_file_base64` and renders with `<img src="data:...">`. Zoom controls: fit-to-window (default), preset steps (10%–500%), +/- buttons.

**Remote files**: SCP commands extracted from the SSH foreground command. Every remote command goes through `util::ssh_target::SshTarget`, which keeps the connect options (`-p`, `-i`, `-J`, `-o`, written as `-o Key=v` so scp reads them too) and muxes over the tab's bridge tunnel socket when there is one — never reduce a command to `user@host` again: a port-only or password-only (bridge-only) host then lists in Files and fails every open. Files >2MB or binary (null bytes in first 8KB) are rejected with a user-friendly error toast.

**Search panel**: Uses `search({ top: true })` — positioned at top of editor. Styled via `:global(.cm-panel.cm-search)` CSS in EditorPane.

**Tab insertion**: New editor tabs insert after the currently active tab, not at the end.

## Diff Tabs

Diff tabs (`tab_type === 'diff'`) render `DiffPane.svelte` using CodeMirror's `MergeView` for side-by-side comparison. Created by Claude Code's `openDiff` tool.

- **Accept**: Writes `new_content` to `file_path` (local or SCP), responds to Claude with success
- **Reject**: Responds to Claude with `DIFF_REJECTED`, closes tab
- **Blocking**: Claude Code waits for the accept/reject response before continuing
- **DiffContext**: `{ request_id, file_path, old_content, new_content, tab_name }`

## Editor Registry

`editorRegistry.svelte.ts` maintains a map of open editor views, used by Claude Code tools to query editor state (dirty tabs, file paths, selections) and by DiffPane for tracking.

## Editor-Specific Pitfalls

- **Capture-phase keyboard shortcuts intercept CodeMirror**: `+layout.svelte` uses `addEventListener('keydown', handler, true)` (capture). For editor-specific shortcuts (Cmd+F, Cmd+K, Cmd+S, Cmd+D), check `activeTabIsEditor` and return early to let events propagate to CodeMirror.
