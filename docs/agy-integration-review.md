# Antigravity (`agy`) integration review

Probed and Phase 1 implemented 2026-11-20 against installed `agy 1.5.13` (Google Antigravity CLI),
its bundled customization docs (`agy-customizations`), and maiTerm's runtime architecture.

## Runtime mapping: `AgentRuntime::Gemini` = `agy`

maiTerm repurposes the existing `AgentRuntime::Gemini` variant (wire key `"gemini"`, session var
`geminiSessionId`, prefs `gemini_*`) for Antigravity CLI (`agy`) instead of introducing a fourth
runtime enum value:

- Prior `gemini-cli` integration was an unimplemented stub (no registrar, no hooks, no MCP, no
  transcripts, auto-resume hardcoded `false`).
- `docs/mailink-protocol.md` already defines `Runtime = 'claude' | 'codex' | 'gemini'`, so keeping
  the wire key `"gemini"` makes `agy` chats work with existing phone builds with zero wire-breaking
  changes.
- User-facing `displayName` is `"Antigravity"`; `relocate/agents.rs`'s existing `~/.gemini/projects.json`
  relocation is independent of `AgentRuntime` and remains intact.

## Live probes captured on `agy 1.5.13` (durable reference)

All probes ran in isolated throwaway git workspaces under `/tmp` with `ANTIGRAVITY_*` scrubbed from
the child environment; no global `~/.gemini` config was modified during probing.

### P1. MCP `clientInfo`, `${VAR}` non-expansion, and stdio env inheritance

- **`clientInfo`**: `{"name": "antigravity-client", "version": "v1.0.0"}` (does NOT contain
  `"gemini"`). `AgentRuntime::detect()` must match `"antigravity"` / `"agy"` / `"gemini"`.
- **HTTP `mcp_config.json` does NOT expand `${VAR}` or `$VAR` in `url` OR `headers`**:
  - `url: "http://127.0.0.1:${PROBE_PORT}/mcp-env-url"` never connected.
  - `headers: {"x-maiterm-tab": "${MAITERM_TAB_ID}", "x-maiterm-authorization": "${MAITERM_AUTH}"}`
    arrived verbatim (`${MAITERM_TAB_ID}` / `${MAITERM_AUTH}`).
  - HTTP transport also sends `POST server/discover` before `initialize` and probes
    `GET /.well-known/oauth-protected-resource`.
- **Stdio MCP servers DO inherit the live PTY environment**:
  A stdio server in `mcp_config.json` received `MAITERM_TAB_ID`, `MAITERM_PORT`, `MAITERM_AUTH`,
  and `PLUGIN_ROOT`, with `cwd` set to the plugin directory.
- **Design consequence (`mcp_stdio.rs` + `agy.rs`)**:
  maiTerm exposes a lightweight stdio-to-HTTP bridge (`aiterm --mcp-stdio`, dispatched in `main()`
  before Tauri starts). The plugin's `mcp-stdio.sh` reads `$MAITERM_BIN` (exported per-PTY) and
  execs `"$MAITERM_BIN" --mcp-stdio`, which forwards JSON-RPC to `http://127.0.0.1:$MAITERM_PORT/mcp`
  with `x-maiterm-authorization`, `x-maiterm-tab`, and sticky `mcp-session-id`. No port or secret is
  written to disk, so dev and prod share one `~/.gemini/config/plugins/maiterm/` plugin cleanly.

### P2. Plugin auto-load (zero trust prompts)

A plugin directory containing `plugin.json`, `mcp_config.json`, `hooks.json`, and
`skills/maiterm/SKILL.md` auto-loaded with no confirmation prompt: MCP tools, hooks, `SessionStart`
priming, and skills were all active immediately.

### P3. Hook trace, `PreToolUse` pass-through trap, and `--conversation` resume

**Fresh session (`agy -p`)**:
```text
SessionStart → PreInvocation(invocationNum=0) → PreToolUse → PostToolUse
             → PostInvocation(invocationNum=0) → PreInvocation(invocationNum=1)
             → PostInvocation(invocationNum=1) → Stop(terminationReason="NO_TOOL_CALL", fullyIdle=true)
```

**Resumed session (`agy --conversation <id> -p`)**:
```text
PreInvocation(invocationNum=0) → PostInvocation(invocationNum=0) → Stop(terminationReason="NO_TOOL_CALL")
```

Critical findings:
1. **`SessionStart` does NOT fire on `--conversation` resume.** Every hook still carries
   `conversationId`, `transcriptPath`, `cwd`, `modelName`, and inherits `$MAITERM_TAB_ID`.
   `hooks_handler` auto-registers unseen `AgentRuntime::Gemini` sessions on their first hook event.
2. **`PreToolUse` returning `{}` HARD-DENIES the tool call** (`tool call denied by pre-tool hook`).
   Returning `{"decision":"allow"}` bypasses user permission prompts. **Returning empty stdout
   (exit 0)** is clean observational pass-through — `agy-hook.sh` MUST print nothing on `PreToolUse`.
3. **All payloads are camelCase protojson**: `hookEventName`, `conversationId`, `transcriptPath`
   (points to `…/.system_generated/logs/transcript_full.jsonl`), `modelName`, `cwd`,
   `toolCall: { id, name, args }`, `terminationReason`, `fullyIdle`, `invocationNum`.
4. **`PreInvocation` fires before EVERY model call in a turn**, not just the user prompt:
   `invocationNum == 0` starts a user turn (`HookPhase::Prompt`); `invocationNum > 0` is a tool-loop
   continuation (`HookPhase::Other`, so Files baselines don't reset mid-turn).
5. **`SessionStart` priming (`format=agy`)**: returning
   `{"injectSteps": [{"systemMessage": {"systemMessage": "..."}}]}` injected into turn 1 context.
6. **Lazy hook startup vs. immediate MCP init**: At idle TUI boot (before the first user message),
   `SessionStart` has not fired yet, while `--mcp-stdio` connects and sends `MCP initialize`
   within ~2s carrying `x-maiterm-tab` (which maiTerm uses to seed an idle session row).

### P4. TUI workspace trust dialog & input box (captured on 100×30 PTY)

- **Trust dialog** (blocks TUI startup in untrusted folders; `--settings '{"trustedWorkspaces":[...]}'`
  bypasses it):
  ```text
  Accessing workspace:
  /path/to/repo
  Do you trust the contents of this project?
  Antigravity CLI requires permission to read, edit, and execute files here.
  > Yes, I trust this folder
    No, exit
    ↑/↓ Navigate · enter Confirm
  ```
  Note: unlike Claude (`No, exit` first), `agy` highlights `Yes, I trust this folder` first.
- **Input box** (same rule-bracketed shape as Claude, using `>` instead of `❯`):
  ```text
  ────────────────────────────────────────────────────
  > Auto mode: edits & safe commands auto-approved (shift+tab to cycle)
  ────────────────────────────────────────────────────
  ```
- **Models & slash commands**: `agy models` prints `<id>\t<display name>` when piped (non-TTY) and
  `<id>  <display name>` on a TTY (with `\r`-prefixed spinner frames); TUI supports `/model`,
  `/effort` (`low|medium|high|xhigh|max`), and `/compact`.
- **Tool-permission prompt (verified live 2026-11-20)**: `agy` has no `PermissionRequest` or
  `Notification` hook; it renders a 6-row numbered prompt (`> 1. Yes, allow tool call` … `6. No, …`)
  ending with `↑/↓ Navigate · tab Amend` above its 1-line `esc to cancel   auto · ...` status bar.
  Pressing a **bare digit (`1`..`6`)** immediately selects and confirms without Enter. maiTerm
  detects it off the live screen (`mailink/permission.rs`), flips `agent_sessions` + frontend state
  to `WaitingPermission` via a per-turn screen watcher (`watch_agy_permission` in `server.rs`), and
  exposes it through `getTabPrompt` / `answerTabPrompt` and Loom/maiLink.
- **Resumed-session priming (`PreInvocation`)**: Because `SessionStart` does not fire on
  `agy --conversation <id>`, `agy-hook.sh` also passes `prime=1&format=agy` on `PreInvocation`
  when `invocationNum == 0` (protojson omits zero fields); `server.rs` tracks primed `(tab, session)`
  pairs and injects `session_priming_text` once on first turn plus Browser Link updates thereafter.

### P5. MCP tool naming, wildcard allow permissions, and always-inlined rules (verified 2026-11-20)

- **Clean `maiterm/<tool>` naming**: Declaring `mcpServers.maiterm` inside a plugin's
  `plugins/maiterm/mcp_config.json` caused `agy` to double-prefix tools as
  `maiterm_maiterm/<tool>`. Declaring it in `~/.gemini/config/mcp_config.json` instead produces
  clean `maiterm/<tool>` names (`ServerName: "maiterm"`).
- **Wildcard MCP allow (`mcp(maiterm/*)`)**: Adding `"mcp(maiterm/*)"` to `permissions.allow` in
  `~/.gemini/antigravity-cli/settings.json` auto-allows all maiTerm tools without per-tool prompts
  (verified live without `--dangerously-skip-permissions`), while `agy`'s Deny > Ask > Allow
  precedence still lets users override any individual tool via `/permissions`.
- **Always-inlined plugin rules**: `agy` defers large MCP tool lists to `<tool>.json` files on
  disk and progressively discloses `skills/`. Putting a compact cheatsheet in
  `plugins/maiterm/rules/AGENTS.md` injects it directly into the system prompt at turn 0.
- **`tools/call` `_meta`**: `agy` also sends `params._meta["antigravity.google/conversation_id"]`
  and `antigravity.google/artifacts_dir` on every MCP `tools/call`.

## Status by Phase

- **Phase 1 (Implemented 2026-11-20)**:
  - `AgentRuntime::Gemini` updated for `agy` (`antigravity-client`, `agy --conversation %geminiSessionId`,
    `"agy"` in `AGENT_PROCESS_NAMES`, `ANTIGRAVITY_*` in `AGENT_ENV_MARKERS`, `MAITERM_BIN` in PTY env).
  - `claude_code/mcp_stdio.rs` (`--mcp-stdio` subcommand) + `claude_code/agy.rs` (`AgyRegistrar`,
    plugin at `~/.gemini/config/plugins/maiterm/` + `agy-hook.sh`).
  - `server.rs` camelCase hook normalization, `PreInvocation(0)` → `Prompt`, `Stop(USER_CANCELED)` →
    `Interrupt`, `Stop(ERROR)` → turn failure, `ask_question` elicitation tracking, resumed-session
    auto-registration, and `format=agy` `injectSteps` priming.
  - `gemini_ide` / `gemini_hooks` / `gemini_auto_resume` / `gemini_ide_ssh` preferences + UI.
- **Phase 2 (Implemented 2026-11-20)**:
  - `mailink/transcript.rs` & `server.rs`: locates `transcript_full.jsonl` (and SSH shadow fallback),
    distills `<USER_REQUEST>` prompts, assistant markdown, unquoted `tool_calls` (including
    `call_mcp_tool` and outgoing `sendToBridgedAgent` peer turns), computes `input_tokens +
    cache_read_tokens` telemetry, 1M Gemini context window, model display/effort parsing,
    `session_is_local`, and Overlord `git commit` confirmation via `GENERIC` output.
  - `agent_files/extract.rs`: extracts `read_file`/`view_file`, `edit_file`/`replace_file_content`,
    `write_file`/`write_to_file`, `grep_search`/`find_by_name`, and `run_command` (`Shell`).
  - `mailink/input_box.rs`, `draft_hold.rs`, `trust.rs`, `mailink/mod.rs`: recognizes `agy`'s `>`
    input box + mode placeholders for draft hold and `await_ready`, and parses `agy`'s workspace
    trust dialog (`Accessing workspace:` + `Do you trust the contents of this project?` + `enter Confirm`).
  - `claude_code/server.rs` & `src/lib/loom/model.ts`: seeds an idle `agent_sessions` row (and emits
    `agent-hook-session-start`) when `agy`'s `--mcp-stdio` bridge sends `MCP initialize` with
    `x-maiterm-tab` so Loom Focus and sidebar dots list live/resumed `agy` tabs before the first
    prompt/hook fires (replaced cleanly on first hook), plus `agy` tool verbs in condensed chat.
- **Phase 3 (Implemented 2026-11-20)**:
  - `claude_code/server.rs` & `scripts/deploy-local-build.sh`: unwraps `call_mcp_tool({ ServerName, ToolName, Arguments })` into `<server>/<tool>` in live hooks, and scrubs `ANTIGRAVITY_*`/`MAITERM_*` env markers during detached local deploy.
  - `claude_code/mcp_stdio.rs`, `claude_code/agy.rs`, `mcp_proxy/entries.rs`, `mcp_proxy/mod.rs`, `agentRequests.svelte.ts`: `--path /mcp/x/<name>` support in the stdio bridge and full `~/.gemini/config/mcp_config.json` sync/collisions/duplicates/adopt (`env.MAITERM_PROXY` marker) for proxied MCP servers.
  - `commands/ssh_tunnel.rs`, `claude_code/agy.rs`, `sshMcpBridge.svelte.ts`, `preferences/+page.svelte`: remote `agy` SSH setup (`build_agy_setup_script`, stdlib-only `mcp-stdio.py` + `mcp-stdio.sh` + remote `mcp_proxy` add-only merge + `settings.json` allow + hooks toggle), and enabled the `gemini_ide_ssh` Preferences toggle.
  - `mailink/models.rs`, `mailink/mod.rs`, `ChatColumn.svelte`: live `agy models` parsing (cached 5m, stripping `\r` spinner frames, fallback `AGY_BUILTIN`), `/model` and `/effort` pickers enabled for `gemini` tabs in Loom and maiLink.
  - `share/mod.rs`, `ShareExportModal.svelte`, `MeshSetupModal.svelte`: `agy` binary & wrapper detection in Workspace Share (`Antigravity` label), `agy|antigravity` generic role detection in `MeshSetupModal`, verified `createTab` (`runtime: "gemini"`).
  - `mailink/subagents.rs` & `mailink/mod.rs`: delegation roster for `agy` `invoke_subagent` (parses `PLANNER_RESPONSE` launches, `GENERIC` `conversationId` acks, `SYSTEM_MESSAGE` completions — both direct `sender=<uuid>` replies and `sender=system` `"Subagent <uuid> has gone idle..."` notices — `send_message` reopen, `manage_subagents` kill, and live child progress from `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript_full.jsonl` with SSH-shadow guard).
