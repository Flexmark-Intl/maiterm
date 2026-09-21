# maiTerm Login — managed Claude Code identities, local and remote

> Status: **partially built**, 2026-09-20 (spec'd 09-19). Owner: Darryl. §13 has what is done.
> Code: `src-tauri/src/accounts/`, `src-tauri/src/commands/accounts.rs`,
> `src/lib/components/accounts/`.
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

### 2.1.1 `CLAUDE_SECURESTORAGE_CONFIG_DIR` — the credential is separable from the config dir

> **Read out of 2.1.278 on 2026-09-19, and confirmed on the wire (macOS).** The blockquote
> above is true only while this variable is unset. The credential store resolves its
> directory through one function, shared by the Keychain path and the `.credentials.json`
> path alike:
>
> ```js
> function qS() {
>   const n = process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR;
>   if (n !== undefined) return (n || join(homedir(), ".claude")).normalize("NFC");
>   return configDir();            // the CLAUDE_CONFIG_DIR-resolved directory
> }
> ```
>
> On macOS the Keychain *service name* is suffixed with `sha256(dir).slice(0,8)` — but the
> suffix is computed from `CLAUDE_SECURESTORAGE_CONFIG_DIR` when that is defined, and is
> **omitted entirely when it is defined-but-empty**. So `CLAUDE_SECURESTORAGE_CONFIG_DIR=`
> (set, empty) pins the credential to the stock location on every platform: the default
> `Claude Code-credentials` Keychain item on macOS, `~/.claude/.credentials.json` on Linux.

Two consequences, and they pull in opposite directions:

- **For §5 (local identities), leave it unset.** Identity isolation depends on the credential
  following `CLAUDE_CONFIG_DIR`, which is the default behaviour. Note the converse risk: a
  user (or a parent process) who exports this variable silently collapses every maiTerm
  identity onto one credential. If §5 ships, scrub it from the spawn environment.
- **For a per-instance *remote* config dir, set it to empty.** That relocates hooks, MCP,
  `projects/` and the rest while the account's real login stays exactly where it is and is
  shared by every instance, unchanged — which is what dissolves the credential-refresh
  question that blocked that work (§5.4, and `dfb1d893` on the board).

It is undocumented, so treat it as version-sensitive: re-verify with the A/B in §5.4 before
relying on it in a release.

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

### 3.5 Not temporarily making maiTerm the default system browser

Considered, to capture a magic link clicked in a mail client (§5.3). Rejected:

- **It is not silent.** On macOS 26 the default handler changes through
  `NSWorkspace.setDefaultApplication(at:toOpenURLsWithScheme:)`, which raises a system
  confirmation dialog; writing `com.apple.launchservices.secure` directly is protected and does
  not reliably take. Two OS prompts per identity added — worse than §5.2, which needs none.
- **A crash leaves the machine reconfigured.** If maiTerm dies between "set" and "revert", every
  link from every app opens in a terminal emulator. maiTerm dying mid-operation is a documented
  state, not a theoretical one.
- **It means becoming a browser permanently.** Eligibility needs `CFBundleURLTypes` for
  http/https in the Info.plist, so maiTerm would appear in every "open with" list and in System
  Settings' browser picker for good — to serve a thirty-second flow.
- **It buys only the magic-link branch.** §5.2 already isolates the main path.

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

The guard is the safety net, not the fix. §5.2 is the fix.

Incidental corroboration of §8, from the URL Claude Code printed: a full `/login` requests
`org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers
user:file_upload user:plugins`, against `setup-token`'s `user:inference` alone.

### 5.2 In-app authentication — no paste, no instructions

Telling the user to sign out of claude.ai, or to shuttle a code into a private window, is a
defect in a setup flow. maiTerm owns a PTY **and** a webview, which is exactly what this needs:

1. Spawn `claude auth login` with `CLAUDE_CONFIG_DIR` set, in a PTY maiTerm already controls.
2. Scrape the authorization URL it prints (`If the browser didn't open, visit: …`).
3. Open that URL in a **Tauri incognito `WebviewWindow`** — a fresh cookie jar, so claude.ai
   renders a real login page instead of resuming the existing session.
4. The user signs in there.
5. The hosted callback at `platform.claude.com/oauth/code/callback` bounces the code to Claude
   Code's **local callback server**, which is on the same machine and therefore reachable from
   the webview.
6. Claude Code receives it, writes the credential into that config dir, prints `Login successful`.
7. maiTerm closes the window and runs the §5.1 duplicate check.

> **Verified 2026-09-19.** `claude auth login` runs a real OAuth callback server:
> `GET http://127.0.0.1:<port>/callback?code=…&state=…` returns `400 Invalid state parameter`
> and the CLI immediately prints `Login failed: Invalid state parameter` and exits. So the
> endpoint exists, validates `state`, and delivering a code to it drives the CLI to completion.
> `incognito(bool)` exists on `WebviewWindowBuilder` in the Tauri 2.11.3 this project already
> depends on.

> **NOT verified, and step 5 above is written more confidently than the evidence supports:
> how the browser reaches that port.** It is *ephemeral* — 60854 on one run, 61026 on the
> next — and the authorization URL contains no port: `redirect_uri` is
> `https://platform.claude.com/oauth/code/callback`, with `code=true` and an opaque random
> `state`. Anthropic's hosted page cannot guess 61026. So either the CLI polls Anthropic for
> the code using `state`, or the hosted page probes localhost, or it decides from **cookies /
> session state set earlier in the flow** whether a local CLI is reachable at all. That last
> one is the working hypothesis, and if it holds, a *fresh incognito context could get the
> long-code form instead of the callback* — precisely the branch this design assumed away.

**This does not sink §5.2, because maiTerm owns the webview.** If the hosted page shows a code
instead of redirecting, maiTerm reads the code out of the DOM and writes it into the PTY at the
`Paste code here if prompted >` prompt. Both branches then end with no user action:

| Hosted page does | maiTerm does |
|---|---|
| Redirects to the local callback | Nothing — Claude Code completes on its own |
| Displays a code | Scrape it from the webview, type it into the PTY |

Designing for both branches is cheaper than establishing which one fires, and it is robust to
Anthropic changing the answer. **Do not build the redirect branch alone.**

### 5.2.1 Built instead: the external private window

§5.2 is still the right end state, but it is not what ships today, and the gap between "the
system browser returns the account you already have" and "an in-app webview" had a cheap middle
that Q7's resolution made viable.

Q7 established that the URL handed to the browser carries
`redirect_uri=http://localhost:<port>/callback`. **Any** browser on this machine can therefore
complete the flow — including a private window of a browser we did not launch the sign-in from.
There is no cookie-driven branch and no code to paste.

So:

1. `login_into_root` drains **both** of the child's streams and extracts the authorization URL
   as it arrives, emitting it on `account-login-url`. Doing it during the wait is the whole
   point: the previous code only read the transcript on timeout, by which point the link has
   expired with the attempt it belonged to.
2. `accounts::browser` holds a table of browsers that accept a private-window switch —
   `--incognito` for the Chromium family, `-private-window` for Firefox, `--inprivate` for Edge
   — resolved against macOS bundle paths, `PATH` on Linux, or `%VAR%`-expanded paths on Windows.
3. The dialog offers **Sign in privately**, which starts the sign-in and opens the link in such
   a window the moment it appears. The browser tab the runtime opens on its own is ignored;
   whichever window completes the flow wins, because both reach the same local callback.

> **This is a convenience, never the mechanism.** Safari has no command-line switch for a
> private window (AppleScript can open one, but not with a URL loading in it, and it trips the
> Automation permission prompt), and neither do Arc or Orion. An empty browser list is a normal
> answer on an ordinary Mac, so the link itself and a **Copy** button are always shown during
> the sign-in — for both buttons, not just the copy one, since someone who clicked plain
> **Sign in** and only then saw the wrong account in the browser needs it just as much.

The authorization URL is not credential material: it is the *start* of an OAuth flow and whoever
opens it still has to authenticate. It is the same string the runtime prints on its own output
for the same purpose. It is still never logged, and `open_private_window` refuses anything that
is not `https://` — the URL becomes an argv entry, and `--user-data-dir=…` arriving there would
hand the "private" window a real profile.

What this does **not** fix, and §5.2 still would: the user must have a supported browser, and a
magic link (§5.3) clicked in a mail client still escapes to the default browser.

### 5.2.2 The runtime prints a different URL than it opens — do not scrape stdout

> **Verified 2026-09-20, 2.1.278.** One `claude auth login`, two URLs, same `client_id` and
> `state`:
>
> | | `redirect_uri` | Result |
> |---|---|---|
> | Handed to the browser | `http://localhost:<port>/callback` | Completes on its own |
> | Printed on stdout | `https://platform.claude.com/oauth/code/callback` | Renders a code to paste |

Q7's answer is right and is *half* the story: reading it as "the URL contains a localhost
redirect" and then taking the URL off stdout produces the paste-code branch — which is
unreachable here, because the child's stdin is `/dev/null`. That shipped, and a private window
duly arrived at "paste this into Claude Code".

The fix falls out of §5.2.1's shim. The runtime opens the browser by shelling out to `open` /
`xdg-open`, so the stand-in on the child's `PATH` **receives the real URL as argv**. It records
it; the poll loop prefers it over anything scraped.

> **So maiTerm decides where a sign-in opens, and the runtime's launcher is shadowed on every
> path, not only the private ones.** `open_with` is `"default"`, a browser id, or absent to open
> nothing. Shadowing it is not only about suppressing an unwanted window — it is the only way to
> observe the URL that works. Stdout scraping remains as the fallback for when no shim can be
> installed (Windows, filesystem error); that is the behaviour that shipped before any of this,
> paste-code branch and all.

### 5.3 The magic-link hole

Claude.ai's email sign-in can deliver a **magic link**, and a link clicked in a mail client
opens in the *system* browser — outside our incognito webview, in the context that already has
a session. Nothing maiTerm does to the webview can prevent that; the mail client is not ours.

Mitigations, in order:

- **Steer to the emailed code, not the link.** The code is typed into the page that is already
  open in our webview, so the flow never leaves. `claude auth login --email <addr>` pre-fills
  the address, which at least starts the user on that path.
- **The §5.1 duplicate guard is the backstop.** If a magic link completes the flow in the
  system browser as the wrong account, the guard catches it after the fact and explains it.
  This is the case the guard exists for, and it is why the guard is mandatory rather than
  belt-and-braces.

> **Honest limit:** maiTerm cannot guarantee isolation for a flow the user completes in another
> application. It can make the in-app path the easy one and detect the bad outcome. That is the
> whole of what is available, and the setup flow should not imply otherwise.

**This preserves §5's principle exactly.** Claude Code still owns the entire exchange — the
URL, the PKCE challenge, the callback server, the credential write. maiTerm chooses the
*rendering context* and nothing else. We never see the authorization code or the credential.

Risks to settle while prototyping, in order of likelihood:

- **Federated sign-in in an embedded webview.** Google refuses OAuth in embedded user agents
  (`disallowed_useragent`), and passkeys in `WKWebView` need particular entitlements. A user
  whose Claude account signs in via Google SSO may be blocked in step 4. This does not sink the
  design — it decides how often the fallback runs.
- **Store isolation across windows.** Two sequential incognito webviews must not share a data
  store, or identity B still resumes identity A's session. Verify before relying on it.
- **Fallback, if either bites:** system browser plus the §5.1 duplicate guard and the §10
  warning. That is a worse experience, not a broken one — and it is still never a paste flow.

### 5.4 `CLAUDE_CONFIG_DIR` moves everything, not just the credential

> **This is the biggest risk in the design and it nearly sinks §5.** The variable relocates the
> whole Claude Code config directory. maiTerm's own integration lives in there.

`~/.claude/settings.json` carries `hooks` — `SessionStart`, `PreToolUse`, `PostToolUse`,
`Stop`, `Notification`, `PreCompact`, `SessionEnd`, `UserPromptSubmit` — plus `permissions`,
`env`, `enabledPlugins` and `statusLine`. The directory also holds `projects/` (the transcripts
maiLink tails and mirrors), `skills/`, `commands/`, `plugins/`, `CLAUDE.md`, `history.jsonl`
and `ide/`.

> **Verified 2026-09-19.** A config dir containing *only* a `settings.json` with a marker
> `SessionStart` hook fired that hook and nothing else — so hooks are read from
> `CLAUDE_CONFIG_DIR/settings.json`, and the user's real `~/.claude/settings.json` is **not**
> consulted. `projectsDirectory` relocates with it, confirmed in the same run.

**`~/.claude.json` relocates too, and it is the one that carries MCP.** It sits at the home
root, not inside `~/.claude/`, so it looks like it should be unaffected — it is not.

> **Verified 2026-09-19.** With `CLAUDE_CONFIG_DIR` set to a fresh directory, Claude Code wrote
> a new `.claude.json` **inside** it and `claude mcp list` reported *"No MCP servers
> configured"* — against five on the default dir, `maiterm` among them. Note this is the file
> that matters: the peer tab established that `mcpServers` in `settings.json` is **not** a
> server source in 2.1.278, so symlinking `settings.json` does not bring MCP with it.

Unmitigated, every managed tab would lose: the `SessionStart` hook that establishes **tab
identity**, **every MCP server including `maiterm` itself** — no `initSession`, no tasks, no
Overlord, no `driveTab` — transcript discovery for maiLink, and every skill, command, plugin,
permission and CLAUDE.md the user has configured. §5 would trade multi-org support for breaking
the product. The MCP half is the worse one: hooks failing is noisy, an absent tool surface is
silent.

**Mitigation, verified to work: a symlink farm.** An identity directory holds symlinks to the
shared configuration and keeps only the credential per-identity.

> **Verified 2026-09-19.** With `settings.json` symlinked into the identity dir, the marker hook
> still fired. With `projects` symlinked back to `~/.claude/projects`, transcripts landed in the
> real directory where maiLink already looks. With `.claude.json` symlinked, all five MCP
> servers came back — `maiterm` connected — and the symlink survived the run intact.

> **The shared set is not static, so reconciling is not a one-off.** maiTerm's own entry in
> `~/.claude.json` is an **ephemeral port and a per-launch auth token**, rewritten at every start
> and re-asserted on a 30s timer when the `claude` CLI clobbers it. A root seeded at account
> creation therefore dials a dead port from the next launch onward — observed 2026-09-20, two
> roots on ports 31271 and 17680 against a live 57173, so every managed tab reported the maiTerm
> MCP server as failed while an unmanaged tab worked. `MergeJson`'s `resync` list existed for
> exactly this and was simply never re-run. `accounts::resync_roots` now re-reconciles every root
> on disk from both places that write the entry. The trigger is *the config changed*, not *a tab
> started*, so it costs nothing per spawn. Hooks need none of this: `settings.json` is a symlink
> and follows on its own.
>
> Two constraints on that merge, both learned the hard way:
> - **A destination that exists but does not parse must be refused, not reseeded.** Seeding means
>   "copy the user's file minus identity", which for an existing account discards its own
>   `oauthAccount` and project state — the very failure this strategy exists to prevent, with no
>   displaced copy to recover from. A partial read of a concurrent write by the co-owning
>   `claude` process looks identical to corruption, so it is transient far more often than
>   terminal: refuse and let the next reconcile try again. Absent still seeds; that is creation.
> - **It runs off the async executor.** One caller is a tokio timer, and this is N × (~200KB read
>   + parse + render + write).
>
> Still open: nothing drift-checks a root's *own* copy, so a managed session that clobbers its
> own `mcpServers` stays on a dead port until the next app start or the next unrelated drift of
> the user's file.

### The contract

An identity root holds symlinks to the user's real `~/.claude` entries, **one merged file**, and
whatever Claude Code creates for itself. Implemented in `src-tauri/src/accounts/mod.rs`, where
`SHARED_ENTRIES` carries a `Strategy` per entry.

| Symlinked (shared with `~/.claude`) | Why |
|---|---|
| `settings.json`, `settings.local.json` | hooks — **tab identity** — permissions, env, statusLine |
| `projects` | transcripts maiLink tails and mirrors |
| `ide` | **maiTerm writes `~/.claude/ide/<port>.lock` here.** Easy to miss, and a managed tab that cannot see it loses IDE discovery |
| `commands`, `skills`, `plugins`, `CLAUDE.md` | the user's own configuration; read-mostly |
| `statusline-command.sh` | referenced by `settings.json`, so it must resolve |

| Merged, per-identity | Why |
|---|---|
| `.claude.json` | **must NOT be a symlink** — see below |

> **`.claude.json` mixes shared config with identity, so it cannot be shared.** It carries
> `mcpServers` (which a managed tab needs, or it loses maiTerm's own bridge) *and* `oauthAccount`
> + `userID`, the record of **which account is signed in**. An earlier draft symlinked it for the
> MCP servers. Caught by testing with two real accounts: both roots reported the **second**
> account's email while holding two different credentials, because the newer sign-in rewrote the
> one shared file. That made §5.1's duplicate key and §6.1's verification both read the wrong
> identity, and — since the runtime writes *through* a symlink — wrote the managed account's
> identity into the user's own `~/.claude.json`, which no discard can undo.
>
> So it is a real per-account file: seeded once from the user's copy minus `oauthAccount` and
> `userID` (project trust and history carry over), then only `mcpServers` refreshed on each
> reconcile. Verified live: a per-account file with `mcpServers` gives a connected `maiterm`
> **and** the correct per-account email.
>
> Two consequences that cost real config before they were guarded:
> - `exclude` applies at **seed time only**. Re-applying it each reconcile deletes the identity
>   the runtime wrote for that account — the thing the strategy exists to protect.
> - **Never run a runtime command against a root whose merged entry is still a symlink.** A root
>   from an earlier build has one, and `auth logout` on the way to deleting it wrote through and
>   stripped the identity block out of the user's real file. `detach_write_through_links` unlinks
>   first. Related and useful: an **absent** `oauthAccount` is refetched on the next session; a
>   **stale** one is not, because its TTL has not expired. That is why one incident self-healed
>   and the other needed a manual `claude auth login`.

| Real, per-identity | Why |
|---|---|
| `.credentials.json` (Linux) | the point of the split, and it **cannot** be a symlink — see below |
| `policy-limits.json`, `remote-settings.json`, `statsig`, `cache`, `backups`, `shell-snapshots`, `debug`, `paste-cache` | Claude Code creates these per-dir on first run; observed appearing in a fresh root. Caches and per-dir bookkeeping, correct to keep separate |

On macOS the credential is in the Keychain keyed by dir path, so the first row costs nothing
there; the split is only load-bearing on Linux.

> **Build the farm as a reconciler, not a one-shot.** The user adds skills and plugins after an
> identity is created, and `~/.claude` gains entries across Claude Code versions. Re-link on
> every spawn and treat an unexpected real file where a symlink belongs as drift to repair —
> that is also how the `mv ~/.claude.json.tmp` hazard below gets absorbed if it ever goes live.
> Drift repair is **non-destructive**: a real file where a symlink belongs is renamed aside, not
> deleted. It may be the only copy of something, and the reconciler is not entitled to that call.

Settled while building:
- **The runtime writes THROUGH a symlink**, on both the `claude mcp add` path and a live `-p`
  session. The link survives while the target's mtime, size *and inode* change — it resolves the
  link, then renames onto the resolved path. That is why a symlink is stable where
  `.credentials.json` is not, and it is exactly why `.claude.json` cannot be one: a write through
  it lands in the user's real file.
- **Cross-owner hazard, conditional (§5.5).** maiTerm's own remote setup writes that file as
  `mv ~/.claude.json.tmp ~/.claude.json` — temp-file-plus-rename **replaces** a symlink instead
  of writing through it. Not live today, because that write targets the *remote* home while
  §5's farm is local. It becomes live the moment a remote config root exists, and the fix is on
  the writer's side. `sshMcpBridge.svelte.ts` also hardcodes `~/.claude.json`, which is correct
  today and wrong the moment a remote root moves.
- **On Linux, `.credentials.json` lives in the dir** and **cannot** be symlinked — this is
  enforced, not merely inadvisable. 2.1.278 opens the credential store with `O_NOFOLLOW` on
  both the read and the write path (`ELOOP` → the internal state `refused-symlink`), and
  `probeCredentials` additionally `lstat`s it and rejects `isSymbolicLink()`. A hard link
  does not rescue it either: the write is temp-file-plus-rename, so the first refresh gives
  the path a new inode and the two names diverge — precisely the mutual-invalidation failure
  we were worried about, with a concrete mechanism. **Use `CLAUDE_SECURESTORAGE_CONFIG_DIR=`
  (§2.1.1) instead of any filesystem trick.** On macOS the credential is in the Keychain
  keyed by dir path, so it stays separate for free.

**The A/B that verifies §2.1.1**, cheap and safe — it touches a throwaway config dir and the
credential store no more than a normal `claude` run does:

```bash
CLAUDE_CONFIG_DIR=$(mktemp -d) claude auth status                                  # control
CLAUDE_CONFIG_DIR=$(mktemp -d) CLAUDE_SECURESTORAGE_CONFIG_DIR= claude auth status # test
```

Read `authMethod`, not `loggedIn` — a stray `ANTHROPIC_API_KEY` (§2.2) makes the control
report `loggedIn: true` with `authMethod: "api_key"`. The test must say `claude.ai`.

> **Verified 2026-09-19 (2.1.278, macOS):** control `api_key`, test `claude.ai`. The Linux
> half is unverified — the classifier blocks running it over SSH from an agent, so it needs
> a human to run it on nova.

> **This is the thing to prototype first.** It also revises the §13 claim that §5 is nearly
> free: the switching mechanism is one environment variable, but making it safe is a directory
> contract that has to be right.

### 5.5 Resolved: no collision with the remote config-root work

`src-tauri/src/claude_code/CLAUDE.md` (~line 671) names per-instance `CLAUDE_CONFIG_DIR` as the
fallback if remote config *convergence* does not hold up, and predicts §5.4's collateral damage.
An earlier draft of this section treated that as a live collision needing a jointly-owned
directory contract. **It is not.** Settled with the remote-config tab, 2026-09-19:

- **Convergence has not failed.** The port ratcheting that motivated the direction change
  (`ews@nova` 28599→28604→28607) turned out to be the `ControlPersist` tunnel leak, not the
  design — fixed in `16b2271`/`edb2811`, shipped 2026-09-15. The one real field incident (remote
  hooks on dead port 40865) was a **peer maiTerm on an old build** overwriting the shared config:
  version skew, not convergence failing on its own terms. Convergence holds once every writer
  converges, and that machine is now current.
- **So the remote split stays parked** — insurance against a writer we do not control, not a
  live defect. That makes this spec the **only** consumer of a config-root split, and it is
  **local-only**.
- **The axes never collided in the filesystem anyway.** §5's roots are local; the remote plan's
  are on the remote host. §6 propagates a token, not a config root, so a managed remote tab
  creates no root at all.
- **There is no identity dimension in the remote design** and adding one would be a rename, not
  an extension. If it is ever un-parked it composes as (identity × instance) with both in the
  remote directory name. **This spec lands first and sets the scheme; the remote work conforms.**

One consequence for §5.4: the cross-owner hazard below (maiTerm's own `mv ~/.claude.json.tmp`
replacing a symlink) is **not live**, because that write targets the *remote* home and §5's farm
is local. It becomes live only if the remote split is un-parked.

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

Host enablement was originally **per host, explicit, and never inferred** — a token is a
standing one-year credential, and `nova` is fine where a shared build box is not our call to
make.

> **Revised 2026-09-21, twice.**
>
> **(a) A catch-all is allowed, because the rule taxed the common case.** One person, one
> account, boxes they own is the normal shape of this, and making them retype every host bought
> nothing — they would have listed all of them anyway, just slower and with one forgotten. So
> `remote_all_hosts` exists, the warning it replaces is stated where it is switched on, and the
> judgement moves from the app to the user.
>
> **(b) Only the ACTIVE account is ever propagated, and that is the load-bearing decision.**
> The same `active_account_ids` pointer that decides what a local tab launches as decides what
> goes to a remote. One identity is current at a time and it is current *everywhere*; switching
> accounts switches local tabs and remote ones together.
>
> This dissolves a problem rather than solving it. The first version searched every account for
> one naming the host, so two accounts could both claim `nova` and something had to arbitrate —
> and per §6.1 a wrong arbitration is invisible, because the tab does not fail, it comes up as
> the other identity. With one account consulted there is nothing to arbitrate: **`remote_hosts`
> answers *whether* to propagate, never *which***. Several accounts may each carry the catch-all
> for that reason; they cannot contend.
>
> So the question at spawn is narrow: does the active account hold a token, and is this host one
> it may reach?
>
> 1. the active account for the runtime, if it has a minted token;
> 2. …and the host is named in its `remote_hosts`, or it has `remote_all_hosts`;
> 3. otherwise **no token at all**, and the host keeps the login it already has.
>
> A bare `nova` matches every user on that host; `ews@nova` matches only that pairing.
> `remote_account_for_host` implements this and is tested, including that a host with no token
> behind it resolves to nothing rather than to an account — a list outliving its token is a
> promise that fails silently.
>
> **Consequence for step 2:** the token must ride the per-tab ssh environment, never the shared
> `~/.aiterm` on the host. That file is one per remote user and outlives a session, so a token
> written there would not follow a switch — the thing this decision exists to guarantee. See
> [[aiterm-file-cross-pollution]] for how the shared-file version of a per-tab value went last
> time.

### 6.1 A missing token silently becomes the wrong identity

The precedence list (§2.2) is a fall-through, and every rung below our token is a *different
account*. This is the sharpest failure mode in the whole design, and it is invisible.

For a managed remote tab, `CLAUDE_CODE_OAUTH_TOKEN` (#5) outranks the host's stored subscription
login (#7), so the injected identity wins and the credential store is never consulted — which is
also why `CLAUDE_SECURESTORAGE_CONFIG_DIR` is irrelevant here, neither helping nor fighting.

**But when the token is absent, expired, or revoked, resolution does not fail. It falls through
to #7** — and the tab comes up as whatever account login happens to exist on that host, with
`loggedIn: true` and no error anywhere. The user selected identity A; the tab is running as
whoever last logged into that box. Work proceeds, billed and attributed to the wrong account.

The same applies above us: a stray `ANTHROPIC_API_KEY` is #3 and outranks our injected token
too. That is not hypothetical — this machine has one, and it surfaced during testing as
`loggedIn: true, authMethod: api_key` on a config dir holding no credential at all.

> **Decision: never treat `loggedIn` as confirmation.** Verify identity **positively** — read
> `claude auth status --json` and compare `email` / `orgId` against the identity that was
> intended. A mismatch is an error state shown on the tab, not a warning in a log.

This is the `absence-read-as-a-claim` defect class the repo has hit six times: a default that is
correct where it is defined, read by a consumer as a positive assertion of something else.
`loggedIn: true` truthfully means "some credential resolved". It does **not** mean "your
credential resolved", and every consumer in this spec wants the second.

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

### 9.4 Revocation — assumed absent (2026-09-21)

`claude auth logout` with the identity's `CLAUDE_CONFIG_DIR` revokes that `/login`
credential. Whether it also revokes outstanding `setup-token` tokens minted from it is
**still unverified** (setup-token saves nothing locally, so there may be nothing for logout to
revoke).

> **Decision: build as if it does not revoke.** This no longer gates §6. The CLI offers no
> revoke of its own — `claude setup-token` has no flags at all and there is no `claude auth
> revoke` — so if logout does not reach these tokens, nothing local does, and a design that
> assumes it might is a design that quietly relies on something we never tested.

Assuming the worse answer is what produces the revoke story, rather than what postpones it:

- **The per-host list is the inventory.** It is the only record of where a standing one-year
  credential has been placed, so it is explicit, per host, and never inferred (§6).
- **Remove and Clear setup must say what they do not do.** They delete the root and the vault
  entry, which stops *maiTerm* handing the token out; they do not promise the token is dead on
  a host that already has it. Saying otherwise would be the `absence-read-as-a-claim` mistake
  again, in the one place where the claim is about security.
- **Expiry carries real weight** (§7). If nothing revokes, `minted_at + 1 year` is the main way
  a token stops working, which makes the T-30d warning part of the security posture and not a
  convenience.
- **Out-of-band revocation is the user's, and we should point at it** rather than pretend it is
  ours.

If Q1 later resolves to "yes, logout revokes", nothing here breaks — maiTerm simply carries
disclosure more conservative than it needed. That asymmetry is the whole reason to assume the
worse answer instead of waiting on the experiment.

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
3. **What it costs** — on this machine, nothing: §8's losses belong to §6 remote propagation,
   and local accounts are full `/login` credentials that lose none of it.

> **Disclose at the point of action, not in advance.** An earlier draft put §8's table here.
> That is wrong while §6 does not exist: it asks the user to weigh a tradeoff they cannot make,
> and by the time they can — a different session, months later — nobody remembers a modal they
> clicked through once. The §8 warning belongs on the control that enables a host, where it is
> true and actionable. Setup says only that accounts are local-only today.
4. **Where credentials live** — OS keychain, never `aiterm-state.json`, never reachable by
   agents over MCP (§9.3).
5. **Remote hosts are opt-in** — a one-year standing credential, enabled per host, never
   inferred.
6. **Authenticate** — runs the flow for the first identity.
7. **Save and enable.**

§5.2's in-app incognito webview is **not built**: sign-in uses the system browser, with §5.2.1's
external private window as the one-click way past the session-reuse trap and the §5.1 duplicate
guard as the safety net behind it.

> **The disclosure above belongs to setup, and only to setup.** The dialog has two modes. The
> seven steps run once, before the feature is on, because that is the only moment the user can
> still decline. Every later **Add account** drops them: by then they have been read and agreed
> to, and repeating them buries the one thing that *is* new each time — the browser is still
> signed in to the account just added, so this sign-in will silently return it (§5.1). The `add`
> body names the accounts already held and offers the two ways through (§5.2.1). Same principle
> as the point-of-action rule above, applied to repetition rather than to timing.

The enable control is the app's switch, not a checkbox — it is a state being turned on, not an
option being selected, and every other toggle in Preferences is a switch.

Refuse setup entirely, with the reason shown, when managed settings are present (§3.4).
**Not built** — there is no managed-settings detector yet, and a fake check would be worse
than none.

**Toggle off** stops injection and leaves identities intact — reversible with one click. The
gate lives in `accounts::spawn_env_for`, not at each call site, so configured-but-disabled
behaves exactly like not-set-up.
**Clear setup** is the destructive one: sign out per identity, delete the config dirs, drop to
*Not set up*. It confirms inline (`window.confirm()` does not work in Tauri webviews).

> **Sign-out is not optional, and belongs in `discard_account_root` rather than in each
> caller.** Deleting a root does not touch the credential: on macOS it is a Keychain item keyed
> by a hash of the config-dir path, so removal alone orphans a still-valid credential under a
> uuid that is never reused — while the confirm text promises the account is gone. Putting it in
> the shared path also covers the §5.1 duplicate discard, which has a credential of its own
> because it signed in. Best effort: a sign-out failure still removes the root, since leaving
> that behind as well would be strictly worse.

> **Removing an account does not stop the sessions already running under it, and they bring the
> root back.** A runtime process holds `CLAUDE_CONFIG_DIR` for its whole life. After
> `remove_root` deletes the tree, that process's next periodic write *recreates* it — a bare real
> directory, no symlink farm — and maiTerm has already dropped the account, so "Clear setup",
> which iterates the account *list*, cannot see it. Observed 2026-09-20: a root still on disk 30
> minutes and two clears after its account was removed, holding a session sidecar. The cost is
> not clutter: a credential refresh from such a session re-mints a Keychain item keyed by the
> deleted path, so an account the user was told is gone survives under a uuid nothing tracks.
> `accounts::prune_orphan_roots` sweeps at startup, where the process responsible has since
> exited. It runs inside `setup()` rather than beside the state load, because the log plugin is
> not active until then and a sweep that deletes directories has to leave a record.
>
> > **The sweep is gated on `state_loaded_successfully()`, and the first version was not.** The
> > safety property written here originally — "preferences are loaded in this same process, so an
> > empty keep-list means *no accounts* and never *not loaded yet*" — is true of **timing** and
> > false of **outcome**. `load_state()` returns `AppData::default()` when the state file is
> > missing, unreadable or unparseable, so an empty `managed_accounts` from that path means "no
> > idea". Ungated, one corrupt launch deleted every account root — in the same second
> > `preserve_corrupt` was saving the file for recovery, leaving restored rows pointing at roots
> > that no longer exist and Keychain items orphaned but still valid, because nothing signed them
> > out. Same flag `save_state` uses to refuse clobbering a known-good backup; the difference is
> > that a backup can be recovered and a config root cannot. **Anything that deletes user data
> > because a collection came back empty must ask this first.**

> **Switching, and turning the feature on or off, reach NEW tabs only — and the pane has to
> offer the remedy, not just state the constraint.** The account is an environment variable
> handed to the shell at exec; nothing can rewrite a running process's environment, and an agent
> already running holds its credential in memory besides. Every other switch in Preferences
> applies immediately, so the one that cannot is exactly the one that must explain itself. A
> notice under the account list was too quiet, and it answered the wrong question — "tabs already
> open keep their account" invites "so how do I move them?". `AccountSwitchModal` says it and
> then lists each window with its workspaces and their live-tab counts, offering a reload per
> window or per workspace, with Close as a first-class outcome. Shared by the switch and the
> toggle, because they are the same situation.
>
> > **Reloading tabs en masse is sharper than it looks**, and three defects came out of it:
> > 1. **Serializing the loop is not enough — the handler must be serialized too.** `listen` does
> >    not await its callback, so two requests to one window ran concurrent loops. `reorder_tabs`
> >    drops any tab id missing from the list it is given, and a second loop's stale list omits
> >    the first's fresh duplicate, so tabs are destroyed. Requests chain onto the previous
> >    promise; the buttons disable while anything is pending.
> > 2. **A replacement tab in a background workspace never mounts.** `TerminalPane` renders only
> >    for the active workspace's tabs, so "Reload all" stops agents elsewhere in the window and
> >    they stay stopped until that workspace is opened. Disclosed rather than prevented — the
> >    tab does come back correctly when visited.
> > 3. **`reloadTab` followed the replacement unconditionally**, which was invisible while every
> >    caller passed the already-active tab. This is the first caller that reloads background
> >    tabs, and it left each pane on whichever live tab came last in the strip.
>
> **Removing the active account must promote a replacement.** Clearing the pointer alone is
> correct bookkeeping and a silent no-op: "active account" then resolves to nothing, new tabs
> quietly use the normal login, and the toggle still claims otherwise. Worst with the feature
> switched off, where nothing surfaces it until it is switched back on and appears broken. The
> first remaining account of that runtime is promoted in the same write, and a runtime left with
> none carries a warning rather than implying one is in force.

> **Destroy AFTER persisting, never before.** `removeAccount` and "Clear setup" used to discard
> the root first. `save()` can be refused — the two-instance conflict guard aborts rather than
> clobber a newer state file — and `setAccountsState` rolls back, so the row returned while its
> root and credential were gone; every new tab would then be handed a `CLAUDE_CONFIG_DIR`
> pointing nowhere and the runtime would recreate it bare. The other order fails safely: a
> leaked root, which the startup prune collects.

> **Apply account changes in ONE write.** Every preferences setter persists the whole object
> through a sync command that clones all app data, so a four-setter change was four full write
> cycles over a multi-megabyte file — and observable half-way. The sequence persisted an account
> row while `accounts_setup_complete` was still false, and an interruption there left the pane
> showing *Not set up* beside a live root none of its controls could reach, because the list,
> Remove and Clear setup all live inside the set-up branch. `setAccountsState` applies any
> combination of the four fields at once.

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
| 1 | Does `claude auth logout` revoke outstanding `setup-token` tokens? If not, what does? | **No longer gates §6** (2026-09-21). Building as if it does not — see §9.4. Verify opportunistically the first time a token is minted for real. |
| 2 | Does `apiKeyHelper` really foreclose subscription auth, empirically? | §3.3 — a 10-minute test; if wrong, the pull model is strictly better |
| 3 | Does `claude setup-token` respect `CLAUDE_CONFIG_DIR` for *which* account it mints against, or does it always re-prompt? | §6 step 1 |
| 6 | Does federated sign-in (Google SSO, passkeys) work inside the incognito webview, and do two sequential incognito windows get separate cookie stores? | §5.2 — decides how often the system-browser fallback runs, and §5.2 is not built yet. §5.2.1 lowers the stakes: an external private window is a real browser, so SSO and passkeys work there today. |

### Resolved 2026-09-19 / 09-20 (2.1.278)

| # | Question | Result |
|---|---|---|
| 4 | Does credential lookup key off `CLAUDE_CONFIG_DIR`? | **Yes, both sides.** Read side: a fresh dir reports `loggedIn: false` rather than finding the default Keychain item. **Write side proven 09-20** with two real accounts: two roots held two distinct credentials. They *reported* the same identity only because `.claude.json` was shared — see §5.4, which is a different bug. Qualified by §2.1.1: this holds only while `CLAUDE_SECURESTORAGE_CONFIG_DIR` is unset. |
| 5 | Does `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` work in a plain SSH shell, and survive repeated invocations? | **Works, but rejected** (§9.2). Verified on macOS and on Linux over SSH. The descriptor is read once: with `exec 3<tok` at shell init the second `claude` silently reports `loggedIn: false`. Gains nothing anyway — `/proc/<pid>/environ` is `0400`, so the env var's only reader is the same user who can read the token file. |
| 7 | How does the browser reach the CLI's ephemeral callback port? | **It is in the URL.** The URL actually handed to the browser carries `redirect_uri=http://localhost:<port>/callback`; only the *printed fallback* uses the hosted `platform.claude.com` callback. So there is no cookie-driven branch to design around, and stdin being null does not break the redirect path — it only makes the paste-code fallback unreachable. Confirmed by a real sign-in completing with stdin null. **This is what makes §5.2.1 possible**: any browser on the machine can finish the flow, so the private window need not be one we own. **And the two URLs are not interchangeable — see §5.2.2.** |

## 13. Build order and status

0. ✅ **Directory contract (§5.4).** Proven, and corrected twice by testing — `.claude.json` is
   merged, not linked, and `ide` had to be added to the shared set.
1. ✅ **Per-tab `CLAUDE_CONFIG_DIR` identities (§5).** `src-tauri/src/accounts/` holds the
   runtime registry and reconciler; the PTY spawn path applies the active account's root and
   scrubs every shadowing variable.
2. ⬜ **Status and expiry (§7).** Not started. `read_account_identity` is the primitive; the
   fleet view across remote hosts is the part that exists nowhere else.
3. 🟡 **Setup lifecycle and pane (§10).** Built, including §5.2.1's external private window and
   copy-link paths, the post-change reload offer, and the orphan-root sweep. Two gaps remain:
   §5.2's in-app incognito webview (sign-in still uses the system browser) and §3.4's
   managed-settings refusal (no detector).
4. ⬜ **Remote propagation (§6).** Not started, still gated on Q1.

**Verified end to end with two real accounts, 2026-09-20.** Two orgs side by side in one window;
distinct account *and* org UUIDs on disk; each root resolving its own identity through the
runtime; `~/.claude.json` untouched; hooks and MCP intact in a managed tab; switch + reload
landing the same workspace on the new account with tools still connected; toggle off falling back
to the unmanaged login; removal and "Clear setup" leaving no roots and — confirmed in Keychain —
no orphaned credentials. The startup sweep collected a root a session had resurrected after its
account was removed.

**Not verified:** anything on a second machine, any runtime but Claude, and whether a `setup-token`
survives `auth logout` (Q1, which gates §6).

## 14. Sources

- [Authentication](https://code.claude.com/docs/en/authentication) — precedence, credential
  storage, `CLAUDE_CONFIG_DIR`, "Generate a long-lived token"
- [Claude apps gateway](https://code.claude.com/docs/en/claude-apps-gateway) ·
  [config](https://code.claude.com/docs/en/claude-apps-gateway-config) — §3.1
- [Remote Control](https://code.claude.com/docs/en/remote-control) — §8
- Binary string inspection, `~/.local/share/claude/versions/2.1.278` — scopes, policy
  strings, `apiKeyHelper` auth-mode message
