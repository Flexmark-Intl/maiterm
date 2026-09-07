# Codex integration review

Reviewed 2026-09-07 against the repository, installed `codex-cli 0.153.4`, official
documentation, and project memory. **All seven findings were addressed the same day.** Each
section below now records what was verified, what changed, and what is still unproven — the
last part matters most, because almost none of this was exercised live.

Implementation progress belongs in maiTerm's **Codex integration follow-up** workstream;
this document records evidence and acceptance criteria, not a second task board.

Two things here are durable reference rather than history, and are worth reading before
touching Codex integration again:

- The **captured hook trace** in C7 — the authoritative event order and payload shapes for
  codex-cli 0.153.4, including that `PermissionRequest` fires before the approval flow decides
  and carries no `tool_use_id`.
- The **URL-expansion probe** in C1 — Codex does not expand `${VAR}` in an MCP `url`, so the
  Claude "every instance writes identical bytes" trick is unavailable, and the MCP entry stays
  per-instance.

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

**Original finding, kept for the record:**

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

### C3. Ineffective settings — FIXED 2026-09-07

`install()` ignored `codex_hooks`, and so did the SSH path (`sshMcpBridge.svelte.ts` gated the
remote Codex setup on `codexIde && codexIdeSsh` alone), so a disabled hooks toggle still
installed hooks on every bridged remote. `codex_hooks_bypass_trust` was persisted and read by
nothing. Those two were the only dead Codex controls — `codex_ide_ssh` and `codex_auto_resume`
are honored — which is exactly what made the group read as working.

Both paths honor `codex_hooks` now, and the OFF branch REMOVES our entries rather than skipping
the write: a toggle the user turns off has to stop Codex reporting, not just stop being
refreshed. Local removal is shared with `unregister`; the remote gets `CODEX_HOOKS_STRIP_PY`,
which takes out only `agent-hook.sh` entries, drops event keys it empties, and leaves a file it
cannot parse alone.

`codex_hooks_bypass_trust` now adds `--dangerously-bypass-hook-trust` to the launch maiTerm
builds, through `resumeCommandFor()` so every call site picks it up. Gated on `codex_hooks` too,
since bypassing trust for hooks that are not installed is meaningless, and it can only ever
affect a launch maiTerm builds — never a `codex` the user starts by hand. It stays opt-in and is
not the remedy for changing definitions; C1 is.

The Preferences copy claiming Codex is "opt-in and disabled by default" is corrected.

### C4. Configuration preservation — FIXED 2026-09-07

All four defects were in what the code DOES to a real file, so all four now have tests that
execute the shipped code against one rather than reading it. `put_codex_mcp_entry` uses
`as_table_like_mut`, accepting and preserving either TOML representation, and refuses (without
writing) a non-table `mcp_servers`. `read_json` reports malformed JSON instead of calling it
absent, logged at error level. `CODEX_TOML_MERGE_PY` recognises whitespace, a trailing comment
and a quoted key, and refuses before writing unless exactly one declaration would result.
`CODEX_HOOKS_MERGE_PY` distinguishes a missing file (start fresh) from an unparseable one
(refuse on stderr). No new remote dependency: still line-based, still no `tomllib`.

**Original finding:**

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

### C5. Lifecycle completion — FIXED 2026-09-07

`Interrupt` and `SessionEnd` are registered, with the per-event 3s timeout Codex caps them at
(registering them at our usual 5s would be rejected). `Interrupt` has its own `HookPhase` and
cancels everything the turn was waiting on — the approval gate C7 files, the tool in flight, any
open ask — and is deliberately NOT treated as a `Stop`: no turn completed, so `finished_a_turn`
stays unset and the tab does not become an unread result. The frontend mirror follows and marks
the tab read, since the human who pressed Esc is looking straight at it.

Not exercised live: a real interrupt, a real session end, and the queued-delivery behaviour
after each.

**Original finding:**

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

### C6. Fork and session ownership — WIRED 2026-09-07, not exercised live

Installed `codex fork --help` confirms `codex fork SESSION_ID`. maiTerm disabled the
capability in its descriptors/adapter, and contested auto-resume returned early for Codex
because the whole model was Claude's appendable flag.

Fork is now a per-runtime spec (`resume.ts`) rather than a flag: Claude appends
`--fork-session`, Codex swaps the `resume` verb for `fork`. `supportsFork` /
`buildForkCommand` / `isForkCommand` / `toForkCommand` all read from it, and the three separate
declarations (TS adapter, TS descriptor, Rust `CODEX_DESC`) agree.

Against the review's requirements: session-ID copying on duplicate/reload is untouched;
`forkResumeIfContested` now produces `codex fork <sid>` when a second tab claims the same
session; `handleEnableAutoResume` recognises that as a fork command and drops it, so the tab
resumes its OWN new id (set from the SessionStart hook) rather than re-forking the original on
every restore. The bridge's tool-capable handshake is preserved by asking a Codex fork to call
`initSession` once — the only signal that proves up + on this instance + tool-capable. A latent
trap was fixed on the way: the Gemini adapter spread Codex's, so enabling Codex fork would have
silently handed Gemini a `codex fork` command.

**Not exercised live.** Unit-tested only (`src/lib/agents/resume.test.ts`). Nobody has run the
picker against a real Codex session, so the fork spawn, the handshake and the first
post-fork resume are unverified end to end.

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
turn-boundary clear closes the window itself.

**Overlay detection is a maintenance liability, and already bit once.** It matches seven header
fragments read out of the 0.153.4 binary, three of them parameterized (`… send input to terminal
{name}?`, `Do you want to approve network access to "{host}"?`, `{tool} needs your approval.`).
The first pass listed only four and missed those three, which turned a live network-access or
named-terminal approval into one the phone refuses to answer — a REGRESSION, since before the
gate every approval was answerable. A Codex TUI rewording does the same thing silently. If this
recurs, prefer over-matching: respondability is only consulted when an approval is actually
outstanding, so a false positive cannot fire on its own.

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
- MCP `url` env expansion probed against a live maiTerm server from an isolated `CODEX_HOME`,
  with a dead-port and a wrong-token control to make the `Auth` column readable. Quoted in C1.
- After all seven fixes: 222 Rust lib tests, 126 Vitest, `npm run check` 0 errors.
- `cargo test --lib codex --offline`: 18 passed.
- Related Vitest suites (`agentDelivery`, `autoResumeContext`, `sshCommand`): 22 passed.
- Executing the actual local TOML helper in isolation reproduced both inline-table panics.
- Executing the actual remote merge snippet against in-memory files reproduced
  duplicate table declarations for a commented header.
- **Live SSH workflows, hook trust across restarts, interruption, the phone permission flow,
  and the Codex fork picker were not exercised.** Unit-test success is not evidence those work,
  and most of what changed here lives on the other side of that boundary. The highest-value
  manual checks, in order: (1) restart maiTerm twice and confirm Codex hooks still fire without
  a new `/hooks` trust prompt (C1); (2) answer a Codex approval from the phone, and confirm an
  auto-approved one stops being answerable (C7); (3) bridge two Codex tabs via the picker (C6);
  (4) run the SSH bridge against `ews@nova` (C1/C3/C4).

Use `ews@nova` for subsequent SSH verification. Back up and restore its Codex
configuration and remove test artifacts. The review itself changed no runtime behavior.
