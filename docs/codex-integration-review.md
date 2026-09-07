# Codex integration review

Reviewed 2026-09-07 against the repository, installed `codex-cli 0.153.4`, official
documentation, and project memory. These are outstanding findings, not shipped fixes.
Implementation progress belongs in maiTerm's **Codex integration follow-up** workstream;
this document records evidence and acceptance criteria, not a second task board.

The user also observed stale maiLink permission cards after automatic approval review
during this documentation pass. C7 is a high-priority correctness issue alongside C1/C2.

## Findings and acceptance criteria

### C1. Shared configuration and hook trust (high priority)

`CodexRegistrar` uses distinct MCP names for dev/prod, but one `agent-hook.sh`, one
prompt, and one matching hook group per event. Installation replaces the group's
token; unregister ignores its port/auth arguments and removes shared hooks/shim.
Both MCP names were present locally during review. Remote Codex setup also bakes
the tunnel port/token into per-account files, so contention extends across machines.

Every local startup generates a fresh auth token and embeds it in the hook command.
Codex trusts the exact hook definition: changing it requires renewed review. A
successful JSON merge therefore does not establish that hooks will execute.
See [OpenAI hook trust documentation](https://learn.chatgpt.com/docs/hooks#review-and-trust-hooks).

Prefer stable shared hook commands resolving credentials and routing from the
owning process. Verify Codex's URL capabilities before selecting a shared MCP
configuration solution; `env_http_headers` support does not prove URL expansion.
Cleanup must preserve other live instances. Retain the existing identity guards;
never solve an absent identity by assigning the active tab or an arbitrary sibling.
The `~/.aiterm` sole-tab gate currently sees only this instance's bridges.

Verify local dev/prod coexistence, quitting either instance, repeated restarts after
trusting hooks, and two machines bridging the same remote account. Do not default
to relocating the entire Codex home or adding repeated config rewrites.

### C2. Startup instructions are missing from the Codex hook (high priority)

`agent-hook.sh` does not request `prime=1`, discards the HTTP response, and returns
`{}`. Session registration still happens, but the task/Overlord text from
`session_priming_text()` never reaches Codex through this hook. Meanwhile
`codex_prompt_body()` still instructs unconditional `initSession`.

Deliver SessionStart priming using Codex's supported hook output, sharing the
server's existing text. Preserve no-op output on observational tool/Stop hooks.
Align the installed prompt with `initSession` being repair-only. Preserve the
Agent Bridge distinction between a hook registration and a tool-capable handshake.
Verify fresh and resumed sessions receive instructions without an opening init call;
check compaction source matching as part of the lifecycle work.

### C3. Ineffective settings (P2)

`install()` ignores `codex_hooks`, and so does the SSH path: `sshMcpBridge.svelte.ts`
gates the remote Codex setup on `codexIde && codexIdeSsh` alone, so a disabled hooks
toggle still installs hooks on every bridged remote. `codex_hooks_bypass_trust` is
persisted but never affects a launch. These two are the only dead Codex controls —
`codex_ide_ssh` and `codex_auto_resume` are honored, so the toggle group reads as
working. Wire the controls through the applicable registration/launch paths, or
remove unsupported controls. Keep bypass opt-in; it is not the remedy for commands
whose definitions change every restart. The Preferences description also incorrectly
calls Codex disabled by default: its actual default is enabled.

### C4. Configuration preservation (P2, reproduced or source-confirmed)

- Local `put_codex_mcp_entry()` panics for valid inline parent or child TOML tables.
  Accept both representations, preserving unrelated settings and comments.
- `read_json()` treats malformed hooks JSON as absent, allowing installation to
  overwrite user hooks. Return a visible parse error and leave the file intact.
- Remote `CODEX_HOOKS_MERGE_PY` fails the same way and is worse: it falls back to an
  empty document on any read error and then rewrites the whole file, so a malformed
  remote `~/.codex/hooks.json` loses every user hook, not just our event groups. Both
  merges need the same refusal, not just the local one.
- Remote `CODEX_TOML_MERGE_PY` matches only an exact header line. A valid
  `[mcp_servers.maiterm] # comment` survives and receives a duplicate table on setup.
  Preserve comments and supported table forms; refuse an unsafe merge before writing.

The documented nova test host runs Python older than 3.11. Do not introduce a
remote `tomllib`/`tomli_w` dependency or require a Python upgrade for this fix.
Prefer a focused compatible merge fix; if parsing is moved locally, preserve the
same remote round-trip and user-config safety requirements.

### C5. Lifecycle completion (P2)

The registrar omits `Interrupt` and `SessionEnd`; the normalizer also ignores
`Interrupt`. Current [Codex hook documentation](https://learn.chatgpt.com/docs/hooks)
supports both. Handle interruption without leaving active/permission state behind;
add termination coverage while retaining process-based cleanup for crashes and
missing hooks. Use each event's supported timeout (Interrupt/SessionEnd cap at 3s),
not the existing general 5s timeout blindly.

Verify state and queued-delivery behavior after interrupt, normal exit, forced
process exit, and restart. A tool-name expiry timer does not clear activity state.

### C6. Fork and session ownership (follow-up capability work)

Installed `codex fork --help` confirms `codex fork SESSION_ID`. maiTerm still disables
the capability in its descriptors/adapter, and contested auto-resume returns early
for Codex because it only models Claude's appendable fork flag.

This needs more than a picker toggle: preserve intentional session-ID copying on
duplicate/reload; create a distinct session when two tabs run concurrently; resume
the new fork's own session thereafter. Preserve tab/task/bridge remapping and the
tool-capable handshake. Do not strip copied IDs or fork again on every restore.

### C7. Auto-resolved approvals remain answerable in maiLink (high priority)

User-observed on 2026-09-07: Codex's current automatic approval reviewer resolves a
permission request, but maiLink still asks the human to answer it.

Source-confirmed state mismatch:
- `normalize_hook_event()` maps every Codex `PermissionRequest` to a human-facing
  permission notification; the handler sets Rust `AgentSessionState::WaitingPermission`.
  An approval request does not establish that the human must act: automatic review
  or another hook may decide it.
- Rust `HookPhase::ToolPost` clears the tool fields but resets `WaitingPermission`
  only for `AskUserQuestion`. Completion of an approved Codex tool leaves it set.
- The frontend PostToolUse listener unconditionally changes its mirror to active,
  so the desktop and Rust state can disagree.
- maiLink's `build_chat_detail()` builds a respondable permission card from the
  Rust state alone. `current_prompt()` also accepts that stale state, and permission
  IDs are only `p_<tab_id>`, not specific to an approval request.

Distinguish actual human-waiting state from automatic review and clear a resolved
request using an authoritative, correlated outcome. Verify what the installed CLI
exposes before selecting the mechanism. Cover automatic allow, automatic deny,
cancel/interrupt, and manual decisions. Do not merely clear every permission on any
PostToolUse: another parallel tool may still have a genuine pending approval. A
denied operation may produce no PostToolUse at all. Do not use a timer or disable
automatic review as the product fix.

Keep Rust state, desktop indicators, maiLink's live pendingPrompt, and response
validation consistent. A resolved/superseded card must not inject an answer into
an ordinary prompt or a different approval. Test this before waiting for the next
unrelated tool or Stop hook, including two overlapping tool requests.

The current production log contains no DEBUG hook sequence for this incident, so
the precise event order was not captured. The state mismatch above is confirmed
from source; full live reproduction and the choice of resolution signal remain pending.

## Constraints from project history

- `x-maiterm-tab` identifies the tab on MCP requests; SessionStart supplies the
  session link. `initSession` repairs identity and is not a routine startup step.
- Stable shared remote config is the current direction. The September 2
  ControlPersist diagnosis supersedes the earlier claim that sibling accounts were
  the dominant cause of port churn; per-instance config roots remain a fallback.
- maiTerm owns task state through MCP. Do not add a Codex rollout task-store mirror.
- Native questions/permissions are the human-attention path. Verify phone flows
  end to end; do not introduce a designed "go to desktop" fallback or status-note channel.
- Keep fixes proportional to the small user base. Rewritten-on-spawn state generally
  needs a clean replacement, not dual-write migration machinery.
- SSH transcript mirroring is currently Claude-only; Codex's snapshot fallback is
  documented scope, not a newly discovered regression in this review.

## Verification recorded

- `cargo test --lib codex --offline`: 18 passed.
- Related Vitest suites (`agentDelivery`, `autoResumeContext`, `sshCommand`): 22 passed.
- Executing the actual local TOML helper in isolation reproduced both inline-table panics.
- Executing the actual remote merge snippet against in-memory files reproduced
  duplicate table declarations for a commented header.
- Live SSH workflows, hook trust across restarts, interruption, and phone workflows
  were not exercised in this review. Unit-test success is not evidence those work.

Use `ews@nova` for subsequent SSH verification. Back up and restore its Codex
configuration and remove test artifacts. The review itself changed no runtime behavior.
