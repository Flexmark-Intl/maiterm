# Codex integration review

Reviewed 2026-09-07 against the repository, installed `codex-cli 0.153.4`, official
documentation, and project memory. C7 is fixed; C1–C6 are outstanding.
Implementation progress belongs in maiTerm's **Codex integration follow-up** workstream;
this document records evidence and acceptance criteria, not a second task board.

C7 (stale maiLink permission cards after automatic approval review) was observed by the user
during the documentation pass and fixed the same day — see that section for the captured hook
trace, which also settles facts C5 depends on. C1 and C2 remain the high-priority pair.

## Findings and acceptance criteria

### C1. Shared configuration and hook trust — HOOKS FIXED 2026-09-07, MCP entry cannot be shared

**Verified first, because the review said to.** Codex does **not** expand `${VAR}` in an MCP
`url`. Probed with an isolated `CODEX_HOME` against a live maiTerm server: a literal correct
port reports `Auth: Unsupported` (it connected; the column is about MCP auth negotiation, and a
deliberately wrong token reports the same), while a literal dead port reports `Auth: Unknown` —
and both `${MAITERM_PORT}` entries report `Unknown`, i.e. no connection. So the Claude solution,
where every instance writes identical bytes because the URL names no port, **is not available
for Codex**. `env_http_headers` working for `x-maiterm-tab` never implied URL expansion, exactly
as the review warned.

**Fixed: the hook definition.** This was the part that actually broke things. The auth token was
baked into the hook command, so every maiTerm launch produced a new definition, Codex marked it
untrusted, and the hooks silently stopped running while the JSON merge kept reporting success.
The command is now `bash "<shim>"` — no token, and locally no port — with the shim reading
`$MAITERM_AUTH` / `$MAITERM_PORT` / `$MAITERM_TAB_ID` from the process, falling back to
`~/.aiterm`. `MAITERM_AUTH` is now exported into local PTYs alongside the other two. Because the
definition is identical for every instance and every launch, dev and prod share one trusted
entry rather than fighting over it, and trust survives restarts. The remote form still bakes the
tunnel port as `$1`: fixed for the life of the install, and not a secret.

`reassert_if_drifted` is implemented now that re-asserting cannot invalidate trust — the merge is
idempotent, so an unchanged file is compared and not written. `unregister` finally uses its
`port` argument: it leaves the shared hooks and shim alone while `another_maiterm_is_live(port)`,
so quitting one instance no longer strips them from under the other.

**Not fixed, and not fixable this way: the MCP entry.** The port must be baked, so
`~/.codex/config.toml` stays per-instance. Dev and prod do not contend (distinct server names),
but two machines bridging one remote account still overwrite each other's
`[mcp_servers.maiterm]`. Options left: a per-instance `CODEX_HOME` (relocates the whole Codex
home — the review says not to default to it), or upstream support for an env-var port.

**Known regression, accepted:** on a remote where the Codex bridge is enabled but the Claude one
is not, nothing writes `~/.aiterm`, so an env-less shell (tmux, `su`) has no `$MAITERM_AUTH` and
its hooks no longer authenticate. They used to, via the baked token. A stable trusted definition
is worth more than that case, and `claude_code_ide_ssh` is on by default.

Original finding follows.

### C1 (original text)

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

### C2. Startup instructions are missing from the Codex hook — FIXED 2026-09-07

`agent-hook.sh` did not request `prime=1`, discarded the HTTP response, and returned `{}`.
Session registration happened, but the task/Overlord text from `session_priming_text()` never
reached Codex. `codex_prompt_body()` also still instructed unconditional `initSession`.

The shim now asks for `prime=1&format=codex` on SessionStart only, matched on the raw payload
(this runs on every hook of every turn, and jq/python are not guaranteed on a remote). The
server returns the shared priming text already wrapped in Codex's documented
`hookSpecificOutput.additionalContext` shape — deliberately server-side, since wrapping a
multi-line string with quotes in it would mean JSON-escaping by hand in bash. Other events keep
discarding their response and answering `{}`, and an unreachable server, unknown tab or timeout
all fall back to `{}`, so a broken maiTerm degrades to "no priming" rather than a malformed hook
reply. The installed prompt now describes `initSession` as repair-only, matching the MCP
`instructions` and the Claude skill.

Not verified: that a fresh and a resumed Codex session actually *show* the instructions in
context. The shim's fail-safe behaviour is tested by executing the shipped script; the delivery
itself is on Codex's side of the boundary. Compaction source matching (`source: "compact"`)
is untested — Codex re-runs SessionStart hooks after a compaction, so priming should re-arrive,
but that was not exercised.

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
supports both, and the C7 trace observed `SessionEnd` firing for real (alongside `Stop`
and `UserPromptSubmit`), so this is a registration gap, not a runtime limitation.
C7's approval clearing depends on it: until `Interrupt` is registered, an interrupted
turn's approvals clear at the following `Stop` rather than at the interrupt. Handle interruption without leaving active/permission state behind;
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

### C7. Auto-resolved approvals remain answerable in maiLink — FIXED 2026-09-07

User-observed on 2026-09-07: Codex's current automatic approval reviewer resolves a
permission request, but maiLink still asks the human to answer it.

**Verified before fixing** (logging hooks on an isolated `CODEX_HOME`, codex-cli 0.153.4,
plus the [hooks reference](https://learn.chatgpt.com/docs/hooks)). One gated Bash call:

```
SessionStart → UserPromptSubmit → PreToolUse(tool_use_id=exec-9244…)
             → PermissionRequest(no tool_use_id) → PostToolUse(exec-9244…) → Stop → SessionEnd
```

- `PermissionRequest` fires *before* the approval flow decides anything, and the hook may
  itself allow/deny/decline. maiTerm returns `{}`, so automatic review then settles it. The
  hook never meant "the human must act" — the root error was mapping it to Claude's
  `permission_prompt` Notification at all.
- `PermissionRequest` carries **no** `tool_use_id`; `PreToolUse` and `PostToolUse` carry the
  same one. `tool_input.description` on the request is Codex's own reason for asking.
- No hook fires when a request is denied or auto-resolved. Approve ⇒ the tool runs ⇒
  `PostToolUse`. Deny ⇒ nothing at all.
- Guardian review took 6.2s; maiTerm sat in `WaitingPermission` throughout and beyond.
- `Stop`, `UserPromptSubmit` and `SessionEnd` all fire for Codex (bears on C5).

**Fixed in `ea5efec` + `224910a`.** `PermissionRequest` has its own `HookPhase` and files a
`PendingApproval` bound to the `tool_use_id` of the matching preceding `PreToolUse`;
`PostToolUse` retires exactly that one, so a gate held for a parallel tool survives an
unrelated completion. `Stop`/`UserPromptSubmit`/a new `turn_id` drop the rest, which is the
backstop for a denial. The session leaves `WaitingPermission` only when nothing is
outstanding, and only for Codex. Respondability is corroborated against the tab's live
viewport (`codex_approval_overlay_open`, failing closed) both when the card is built and
again inside `respond_to_prompt` before injecting; prompt ids became `p_<tab>_<seq>`.
`PostToolUse` carries `approvals_open` so the frontend mirror stops disagreeing with Rust.

**Residual, deliberately not chased:** Codex reports a denial through no hook, and the
overlay text lingers for a redraw, so for a moment after a denial the viewport check still
reads true. The per-request prompt id stops that window answering a *later* approval, and the
turn-boundary clear closes the window itself. Overlay detection is also text-based against
0.153.4's four headers — a Codex TUI rewording breaks it toward "not respondable", never
toward a stray keystroke.

Original source-confirmed state mismatch, kept for the record:
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

Acceptance as met: automatic allow retires the approval on its own `PostToolUse`; automatic
deny and manual deny fall to the turn boundary; a manual approval retires like any other.
No timer is involved and automatic review is untouched. Parallel approvals are independent
(`post_tool_use_retires_only_the_approval_it_belongs_to`,
`a_bound_approval_is_never_retired_by_the_fallback`), and clearing does not wait for an
unrelated tool or for `Stop`. Unit coverage lives in `claude_code::server::tests` and
`mailink::tests`; 208 Rust and 113 Vitest tests pass.

Still open on this finding: **interrupt is not covered end to end**, because the registrar
does not install Codex's `Interrupt` hook at all — that is C5. Until it lands, an interrupted
turn's approvals clear at the following `Stop` rather than at the interrupt. **The live
phone-side flow was not exercised**; the fix is verified by unit tests, the captured hook
trace, and source reading, and per project rule that is not evidence the phone flow works.

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

- Codex hook order and payload schema captured live from codex-cli 0.153.4: logging command
  hooks under an isolated `CODEX_HOME`, one `codex exec` triggering a sandbox escalation.
  That trace is quoted in C7 and is the basis of its fix.
- After the C7 fix: 208 Rust lib tests, 113 Vitest, `npm run check` 0 errors.
- `cargo test --lib codex --offline`: 18 passed.
- Related Vitest suites (`agentDelivery`, `autoResumeContext`, `sshCommand`): 22 passed.
- Executing the actual local TOML helper in isolation reproduced both inline-table panics.
- Executing the actual remote merge snippet against in-memory files reproduced
  duplicate table declarations for a commented header.
- Live SSH workflows, hook trust across restarts, interruption, and phone workflows
  were not exercised in this review. Unit-test success is not evidence those work.

Use `ews@nova` for subsequent SSH verification. Back up and restore its Codex
configuration and remove test artifacts. The review itself changed no runtime behavior.
