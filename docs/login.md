# maiTerm Login — managed Claude Code identities, local and remote

> Status: **proposed, not built**, 2026-09-19. Owner: Darryl.
> Scope: maiTerm optionally holds N Claude subscription identities, runs the auth flow
> itself, serves the right identity to each tab (local and over SSH), monitors expiry
> across the fleet, and exposes switching to maiLink. Opt-in, off by default, and
> explicitly **not for enterprise/managed machines** (§3.4).
> Ground truth in §2 was verified against Claude Code **2.1.278** on 2026-09-19 — re-verify
> before building, this surface has moved twice in 2026.

## 1. Why

Three problems, all of which maiTerm is uniquely placed to see:

- **Claude Code has one login slot.** Working across two orgs means `/logout`, `/login`,
  browser, repeat — and it stomps every tab at once, because the credential is global to
  the config dir. Multiple subscriptions is a core requirement here, not a nicety.
- **Remote logins expire silently.** Each SSH host holds its own credential. Nobody is
  looking at `nova` on a Tuesday; you find out when a tab 401s mid-task. maiTerm already
  knows every host it has a tab on, so it is the only thing that *could* watch them.
- **There is no fleet view of auth.** Claude Code 2.1.210+ warns 3 days before a local
  login expires and shows a `Login` row in `/status` — but only for the machine you are
  sitting at. Across hosts, nothing exists.

## 2. Ground truth (verified against 2.1.278, 2026-09-19)

### 2.1 Where the credential lives

| Platform | Store |
|---|---|
| macOS | Keychain, generic password, service `Claude Code-credentials`, account = local username |
| macOS (fallback) | `~/.claude/.credentials.json`, mode `0600`, when the Keychain rejects the write — **notably in an SSH session, where the Keychain is locked** |
| Linux | `~/.claude/.credentials.json`, mode `0600` |
| Windows | `%USERPROFILE%\.claude\.credentials.json` |

The Keychain entry's `mdat` is rewritten on every token refresh, so it is a free
change-detection hook if we ever want one.

> **`CLAUDE_CONFIG_DIR` relocates the `.credentials.json` *and keys the macOS Keychain
> entry to that directory*.** A session with a different `CLAUDE_CONFIG_DIR` reads a
> different credential. This is the whole basis of §5 — native multi-identity, no keychain
> surgery, no mutation of a shared slot.

**There is no switch to force the JSON path on macOS.** Searched 2.1.278 for every plausible
shape (`*KEYCHAIN*`, `useKeychain`, `disableKeychain`, `skipKeychain`) — nothing. The JSON
fallback is failure-triggered only. Do not try to induce the failure to get a uniform code
path across platforms: deliberately breaking a security mechanism to reach a fallback is a
patch release away from breaking. On macOS the local store is the Keychain, and §5 is built
so that this is not our problem.

### 2.2 Credential precedence (docs: Authentication → Authentication precedence)

1. Cloud provider (`CLAUDE_CODE_USE_BEDROCK` / `_VERTEX` / `_FOUNDRY`)
2. `ANTHROPIC_AUTH_TOKEN`
3. `ANTHROPIC_API_KEY`
4. `apiKeyHelper`
5. **`CLAUDE_CODE_OAUTH_TOKEN`** ← what we inject on remotes (§6)
6. Anthropic profile / federation credentials
7. Subscription OAuth from `/login` ← the default, and what §5 switches between

A signed-in gateway session sits outside the list and outranks everything.

Consequence worth designing around: **a stray `ANTHROPIC_API_KEY` in the user's shell
profile outranks our injected token.** This machine currently reports
`apiKeySource: ANTHROPIC_API_KEY` alongside `authMethod: claude.ai`, so this is not
hypothetical. Any status UI must show the *resolved* source, never a green "logged in".

### 2.3 What `claude auth status --json` gives us

`loggedIn`, `authMethod`, `apiProvider`, `analyticsDisabled`, `projectsDirectory`,
`configDirectory`, `apiKeySource`, `email`, `orgId`, `orgName`, `subscriptionType`.

**No expiry field.** See §7 for why we derive expiry instead of parsing for it.

### 2.4 `claude setup-token`

- Opens the same browser flow as `/login`; prints an `sk-ant-oat01-…` token to the terminal
  and **saves it nowhere**. Whoever runs it must capture stdout.
- **Valid one year. Does not rotate.**
- Requires a Pro / Max / Team / Enterprise plan; authenticates against the subscription.
- Consumed as `CLAUDE_CODE_OAUTH_TOKEN`. A `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`
  variant exists (§9.2).
- **Inference-only scope** — see §8.
- Bare mode (`--bare`) does not read it.

## 3. What we are not doing, and why

### 3.1 Not the Claude apps gateway

`claude gateway` looked like the ideal answer — one credential holder, N credential-less
consumers. It is not. Its upstreams are `anthropic` (API key), `bedrock`, `anthropicAws`,
`vertex`, `foundry`. **A Claude.ai Pro/Max subscription is not a supported upstream**, and
the `oidc` block is mandatory — there is no static-token client mode. It solves "500 devs
and a Bedrock account", not "my subscription should follow me to my SSH boxes". Closed.

### 3.2 Not copying the interactive credential blob to remotes

The `/login` credential holds a **rotating** refresh token (the binary carries
`refreshTokenExpiresAt`, `refresh_token_expires_in`, and a `refresh_token_dead` state).
Copy one blob to N hosts and each host's `claude` refreshes on its own timer; the first
refresh invalidates the rest. That is a stable first hour followed by a fleet that logs
itself out intermittently — worse than no feature, because the failure is non-obvious.

### 3.3 Not a pull model via `apiKeyHelper`

Tempting: the remote holds nothing and fetches the token from maiTerm over the SSH tunnel
that already exists. But configuring `apiKeyHelper` flips the session into API-key auth —
the binary says so in as many words (*"apiKeyHelper is configured, so this session is using
API-key auth"*) — so it cannot carry subscription entitlement. Worth a 10-minute empirical
check before we discard it permanently (§12), but do not plan on it.

### 3.4 Not for enterprise or managed machines

- Managed policy can forbid the whole thing: *"setup-token creates a long-lived Claude.ai
  subscription token, which this policy does not permit — use an API key instead."*
- `claude setup-token` enforces `forceLoginMethod` but **not** `forceLoginOrgUUID`, so it
  can mint a token in a different organization than an admin intended. We must not automate
  around an admin control.

> **Decision:** if any managed-settings source is present on the machine, the feature
> refuses to enable and says why. This is a solo/small-team feature.

## 4. Shape

```
Preferences.login_management            ← off by default; setup is a separate artifact (§10)
  ├─ Identity vault (OS keychain)       ← N identities, credential material NEVER in state JSON
  ├─ Per-tab CLAUDE_CONFIG_DIR          ← local switching, concurrent, zero mutation (§5)
  ├─ Per-host CLAUDE_CODE_OAUTH_TOKEN   ← remote propagation, opt-in per host (§6)
  ├─ Fleet status + expiry watch        ← local + every SSH host we have a tab on (§7)
  ├─ Sidebar accessor + switcher        ← humans
  └─ maiLink                            ← status + switch + relogin, in-app only (§11)
```

## 5. Local identities — per-tab `CLAUDE_CONFIG_DIR`

> **maiTerm owns the directory. Claude Code owns the credential inside it.**
>
> This is the load-bearing distinction in the whole design, so state it before the mechanism:
> maiTerm **never** reads, writes, parses, transports, or migrates a `/login` credential, and
> **never touches Claude Code's Keychain item**. We set one environment variable and Claude
> Code does its own browser flow, its own storage, its own refresh, its own logout. We are not
> a party to that Keychain item, so there is no shared ACL, no prompt storm, and nothing of
> ours to break when Anthropic changes the credential format.
>
> Any future change that has maiTerm handling local credential material directly is not an
> extension of this design — it is a different one, and needs its own security review.

Each identity owns a config directory under maiTerm's data dir:

```
<app_data>/logins/<identity_id>/        ← CLAUDE_CONFIG_DIR for that identity
```

Spawning a tab sets `CLAUDE_CONFIG_DIR` to the identity bound to that tab, falling back to
the workspace default, falling back to the global active identity. Unset means "leave
Claude Code alone", which is what an unmanaged tab gets.

Why this beats swapping the Keychain entry:

- **No mutation of shared state.** We never write the user's `Claude Code-credentials`
  entry, so an unmanaged tab and a maiTerm-managed tab coexist.
- **Concurrent, not global.** Two tabs can run two different orgs *at the same time*. A
  keychain swap could only ever be "switch the active one", and would not affect already
  running sessions anyway (the token is in process memory).
- **Logout is scoped.** `claude auth logout` with that `CLAUDE_CONFIG_DIR` set removes and
  revokes only that identity.

Adding an identity: spawn `claude auth login` with `CLAUDE_CONFIG_DIR` pointed at a fresh
directory. Claude Code drives its own browser flow; we never see or handle the credential.
Read back `claude auth status --json` in that dir to learn `email` / `orgName` /
`subscriptionType` and label the identity.

> **Verified 2026-09-19 (2.1.278, macOS).** A fresh `CLAUDE_CONFIG_DIR` reports
> `loggedIn: false, authMethod: "none"` while the default dir reports the real `claude.ai`
> login with `email` / `orgId` / `orgName` / `subscriptionType: "max"`. The credential lookup
> **is** keyed by config dir — it did not fall through to the default Keychain item. This is
> the read side, which is the side §5 depends on; the write side (a second login creating a
> second Keychain item) still needs a real second account to confirm.

### 5.1 The duplicate-identity trap — a required guard, not an edge case

> **Observed 2026-09-19.** `claude auth login` into a fresh config dir opened the system
> browser, claude.ai **reused the existing session cookie**, and the login completed as the
> *same account* with no account chooser. Claude Code printed `Login successful`. Both config
> dirs then reported the identical `email` and `orgName`.

This is the single most likely way the core feature fails in the user's hands: "add a second
identity" silently produces a second copy of the first, switching appears to do nothing, and
nothing anywhere reports an error. It is not a testing artifact — a user adding their second
org will be signed into their first in that browser essentially always.

> **Decision: after every `claude auth login`, read back `claude auth status --json` in the
> new dir and compare `email` + `orgId` against every existing identity. On a match, discard
> the directory and tell the user what happened.** An identity list is not allowed to contain
> two rows for the same account.

The message has to name the cause and the fix — sign out of claude.ai first, or use a private
window — because "login successful" has already told the user the opposite.

Better, if it proves workable: Claude Code prints the authorization URL to stdout
(`If the browser didn't open, visit: …`). maiTerm can capture that and open it in an isolated
context — a private window, or its own webview with a separate cookie store — so each identity
authenticates cleanly instead of relying on the user to remember. Worth prototyping in step 1;
the detection guard above is mandatory regardless, since it is what catches the failure when
the isolation does not hold.

Incidental corroboration of §8, from the URL Claude Code printed: a full `/login` requests
`org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers
user:file_upload user:plugins`, against `setup-token`'s `user:inference` alone.

Note what this means for the macOS Keychain question (§2.1): on macOS these identities *will*
use the Keychain, one item per config dir, and that is fine — **we never read them**. Wanting
uniformity with the Linux JSON path is not a reason to intervene, because there is nothing
here for us to unify: on every platform, the local credential is a thing Claude Code puts in
a directory we happen to own.

## 6. Remote propagation — `setup-token`

Per identity, per host, opt-in:

1. Mint once, locally: `claude setup-token` with that identity's `CLAUDE_CONFIG_DIR`.
   Capture stdout, store in maiTerm's vault (§9.1), record `minted_at`.
2. On SSH tab spawn for an enabled host, inject `CLAUDE_CODE_OAUTH_TOKEN` through the
   existing env-injection path. See §9.2 — the fd alternative was tested and rejected as the
   default.

Because the token does not rotate, N hosts sharing one token is safe and concurrent — this
is exactly the property §3.2 lacks.

> **Do not try to write the remote's `.credentials.json` instead.** That file holds the
> rotating OAuth blob, not an `oat` token — synthesizing it means reverse-engineering an
> undocumented format *and* re-inheriting the rotation problem from §3.2. The remote store
> being a JSON file on Linux (and on macOS over SSH, where the Keychain is locked) does not
> make it a thing we can write.

Host enablement is **per host, explicit, and never inferred**. A token is a standing
one-year credential; `nova` is fine, a shared build box is not our call to make.

## 7. Expiry and fleet status

> **Decision: derive expiry from our own mint record; never parse the credential blob.**

The blob is an undocumented private format and `claude auth status --json` does not expose
expiry, so parsing is the only way to get it — and it would break without warning. Instead:

- **Remote (oat tokens)**: we minted them, so we know `minted_at + 1 year`. Exact, free,
  and no private formats. Warn at T-30d; this is the silent-failure case that motivated the
  feature, and a one-year token fails around month eleven on a box nobody is looking at.
- **Remote liveness**: `claude auth status --json` over SSH per host, cached, cheap. Gives
  `loggedIn` and the resolved `apiKeySource` — which catches the §2.2 shadowing case.
- **Local**: we do not have expiry and will not fake it. Claude Code's own 3-day warning
  and `/status` row cover the machine the user is sitting at. Show "managed by Claude Code"
  for the local slot rather than inventing a number.

## 8. What this intentionally breaks

A `setup-token` session is **inference-only** (`user:inference`, versus a full login's
`user:inference user:sessions:claude_code user:mcp_servers`). On any host authed this way:

| | Status |
|---|---|
| Model requests | ✅ works |
| **Locally-configured MCP servers** | ✅ **works** — maiTerm's own bridge is unaffected |
| Remote Control sessions | ❌ unavailable |
| claude.ai connectors | ❌ unavailable |
| `--bare` sessions | ❌ does not read the variable |

**This is a deliberate trade, not a regression.** Losing Remote Control and claude.ai
connectors on remote hosts is acceptable — desirable, even — but it must be stated plainly
in the setup modal (§10) rather than discovered. A user who wants those features should not
enable remote propagation.

Note the asymmetry: §5 local identities are full `/login` credentials and lose nothing.
Only §6 remote propagation is scoped down.

## 9. Security posture

### 9.1 Storage

> **Credential material never touches `aiterm-state.json`.** It is plaintext on disk and
> is the file we tell people to inspect when debugging.

Tokens go in the OS keychain **under maiTerm's own service name** — our item, our ACL, never
Claude Code's (§5). A signed and notarized app reads its own item without prompting, so none
of the objections to touching someone else's Keychain entry apply.

State JSON holds only identity metadata: id, label, email, org, `minted_at`, which hosts are
enabled. If the keychain is unavailable, the feature degrades to local-only (§5) rather than
silently falling back to a plaintext file.

**Why not mirror the Linux `.credentials.json` pattern and keep it simple?** Because the
`oat` token is the highest-value secret in this design — one year, non-rotating, unlocks the
subscription — and it is the one piece maiTerm actually holds. Storing it in plaintext beside
`aiterm-state.json` would be a downgrade on precisely the wrong secret, chosen for symmetry
with a platform that has no encrypted store to use.

### 9.2 In transit and at rest on the remote

SSH protects the wire. On the remote, the token is a standing one-year credential at rest.

**The fd alternative was tested and is not worth it.** `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`
works — verified on macOS and on a Linux host over a plain SSH shell, both reporting
`authMethod: "oauth_token"` identically to the env var, so the `CLAUDE_CODE_REMOTE` /
`process.send` bypass does not apply to an SSH shell. It was rejected on two grounds:

- **It buys almost nothing.** The argument for it was that `/proc/<pid>/environ` leaks the env
  var. But that file is mode `0400`, owned by the process owner — verified. The only reader is
  the same user, who can equally read the `0600` token file the fd needs. There is no threat
  model where the fd wins and the file does not lose.
- **It has a silent-failure footgun.** The descriptor is read **once**. With `exec 3<tok` set
  up at shell init, the first `claude` invocation authenticates and **every subsequent one
  silently reports `loggedIn: false`** — confirmed. Only a per-command redirect
  (`claude … 3<tok`) survives repeated use, which cannot be expressed as a plain `export` and
  would need a shell wrapper that anything invoking `claude` directly would bypass.

> **Decision: plain `CLAUDE_CODE_OAUTH_TOKEN`.** An intermittent auth failure that appears on
> the second invocation is precisely the class of bug this whole feature exists to eliminate.
> Do not reintroduce the fd form without solving the single-read problem for subprocesses.

Residual exposure to accept and document in the setup modal: the token is inherited by child
processes of the shell, and may appear in crash dumps or debug logs that capture the
environment. Both are same-user exposures, consistent with the file-based alternative.

### 9.3 Agents must never reach credential material

Claude Code's own auto-mode classifier blocks agents from reading credential files
(*"Credential Materialization"*) — correctly. If maiTerm's MCP surface exposed tokens it
would be a neat bypass of exactly the protection the runtime is providing.

> **Decision:** the MCP surface is **status only** — which identity is active, whether a
> host is authed, when it expires. No tool returns, accepts, or logs token material, and
> `switchIdentity` (if we ship it at all) names an identity by id, never a credential.

### 9.4 Revocation — open

`claude auth logout` with the identity's `CLAUDE_CONFIG_DIR` revokes that `/login`
credential. Whether it also revokes outstanding `setup-token` tokens minted from it is
**unverified** (setup-token saves nothing locally, so there may be nothing for logout to
revoke). See §12 — this gates §6 shipping.

## 10. Lifecycle: setup / enable / disable / clear

Four states, not two. A bare on/off toggle is wrong because turning it on the first time
has to *teach* and *authenticate* before it can mean anything.

| State | Control shown | Meaning |
|---|---|---|
| Not set up | **Set up…** | Feature dormant, nothing stored |
| Set up, enabled | Toggle (on) + **Clear setup** | Identities in force |
| Set up, disabled | Toggle (off) + **Clear setup** | Identities retained, nothing injected |
| — | **Clear setup** | Revokes, deletes identities, returns to *Not set up* |

**Setup modal**, in order:

1. **What this does** — maiTerm holds your Claude logins, switches between them per tab,
   and keeps your SSH hosts signed in.
2. **What it does not do** — never parses or stores your local login; local identities are
   directories Claude Code owns (§5).
3. **What it costs** — the §8 table, stated plainly, with the remote/local asymmetry
   spelled out.
4. **Where credentials live** — OS keychain, never `aiterm-state.json`, never reachable by
   agents over MCP (§9.3).
5. **Remote hosts are opt-in** — a one-year standing credential, enabled per host, never
   inferred.
6. **Authenticate** — runs the flow for the first identity.
7. **Save and enable.**

Adding a *second* identity later needs its own warning, immediately before the browser opens:
**sign out of claude.ai or use a private window, or you will sign in as the account you
already have** (§5.1). The duplicate guard catches it either way, but a user who reads this
first does not have to hit the failure to learn it.

Refuse setup entirely, with the reason shown, when managed settings are present (§3.4).

**Toggle off** stops injection and leaves identities intact — reversible with one click.
**Clear setup** is the destructive one: `claude auth logout` per identity, purge the vault,
delete the config dirs, drop to *Not set up*. It confirms inline (`window.confirm()` does
not work in Tauri webviews) and names what it is about to revoke.

## 11. maiLink

Status, switch, and relogin. Per the standing rule, every flow completes **in-app** — no
"go to the desktop". For an OAuth browser flow that means the phone opens the auth URL
itself and the desktop captures the callback.

The phone never receives credential material, only identity metadata and expiry (§9.3).
Wire changes bump `protocolVersion` per `docs/mailink-protocol.md` §13.5, additive
included.

## 12. Open questions — resolve before building the section that depends on them

| # | Question | Gates |
|---|---|---|
| 1 | Does `claude auth logout` revoke outstanding `setup-token` tokens? If not, what does? | §6 — do not ship remote propagation without a revoke story |
| 2 | Does `apiKeyHelper` really foreclose subscription auth, empirically? | §3.3 — a 10-minute test; if wrong, the pull model is strictly better |
| 3 | Does `claude setup-token` respect `CLAUDE_CONFIG_DIR` for *which* account it mints against, or does it always re-prompt? | §6 step 1 |
| 6 | Can maiTerm open the authorization URL in an isolated browser context (private window, or its own webview with a separate cookie store) so each identity authenticates cleanly? | §5.1 — a UX improvement, not a correctness gate; the duplicate-detection guard is required either way |
### Resolved 2026-09-19 (2.1.278)

| # | Question | Result |
|---|---|---|
| 4 | Does credential lookup key off `CLAUDE_CONFIG_DIR`? | **Yes**, on the read side — a fresh dir reports `loggedIn: false` rather than finding the default Keychain item (§5). **Write side still unproven**: the first attempt authenticated the *same* account because the browser reused its claude.ai session (§5.1), so both dirs held one identity. Retest needs a private window or a signed-out browser. |
| 5 | Does `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` work in a plain SSH shell, and survive repeated invocations? | **Works, but rejected** (§9.2). Verified on macOS and on Linux over SSH. The descriptor is read once: with `exec 3<tok` at shell init the second `claude` silently reports `loggedIn: false`. Gains nothing anyway — `/proc/<pid>/environ` is `0400`, so the env var's only reader is the same user who can read the token file. |

## 13. Build order

1. **Per-tab `CLAUDE_CONFIG_DIR` identities (§5).** Cheapest, most differentiated, no
   credential handling at all, and it delivers the core requirement — multiple orgs, side
   by side. Resolves Q4 on contact.
2. **Status and expiry (§7).** Folds in naturally; the fleet view is the part that exists
   nowhere else.
3. **Setup lifecycle and modal (§10).** Required before either of the above ships to
   users, even though it is built third.
4. **Remote propagation (§6).** Last, gated on Q1, per-host opt-in, with §8 shown per host.

## 14. Sources

- [Authentication](https://code.claude.com/docs/en/authentication) — precedence, credential
  storage, `CLAUDE_CONFIG_DIR`, "Generate a long-lived token"
- [Claude apps gateway](https://code.claude.com/docs/en/claude-apps-gateway) ·
  [config](https://code.claude.com/docs/en/claude-apps-gateway-config) — §3.1
- [Remote Control](https://code.claude.com/docs/en/remote-control) — §8
- Binary string inspection, `~/.local/share/claude/versions/2.1.278` — scopes, policy
  strings, `apiKeyHelper` auth-mode message
