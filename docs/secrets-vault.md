# Secrets Vault: keys agents can use but never see

2026-10-22. Design only, nothing is built yet. Darryl: "It's important to keep secrets out of
sessions, yet it's very handy for agents to be able to pipe them where they need to be." Also
on the list: key rotation, "a fairly intensive process" for developers, and dev/prod key sets.
The shape Darryl proposed is the spine of this design. An agent declares the key **names** it
needs and the **platforms** it may deploy to, pushes up a UI where the human types the values,
and is told when the human clicks Done. An agent can also ask to rotate keys: "The agent has
requested to rotate these keys: a, b, c. Do you want to continue?"

## 1. The rule

> **An agent handles a secret's name, never its value. Rust looks up the value at the moment
> it is used, and sends it to a place the human approved.**

Everything else follows from this:

1. **No MCP tool returns a value, and no tool accepts one.** Values reach the vault in four
   ways only: the human types them into maiTerm's own entry sheet, Rust generates them, Rust
   captures them from an approved command's output, or Rust reads them out of a file (§6).
2. **A value never passes through a PTY, argv, a log line, `aiterm-state.json`, the phone, or
   a share file.** The remote setup-token already works this way (docs/login.md §9,
   `accounts/vault.rs`). This feature makes that pattern general.
3. **Secrets are tied to destinations.** Each key and environment lists where it may go. A
   prompt-injected agent's natural move is "send STRIPE_KEY to this URL". The defense is that
   no tool takes an arbitrary URL. Masking output is a backstop, not the protection.
4. **maiTerm knows every place it put a value.** Each delivery is recorded in a ledger (§4.5).
   That ledger is what makes rotation tractable: today the slow part of rotating a key is
   finding everywhere it lives.

## 2. How the pros do it, and what we take

| Tool | The idea worth taking |
|---|---|
| **Doppler / Infisical** | A project has **environments** (dev / staging / prod), and each environment has a full key set built from the same list of names, so "missing in prod" is visible at a glance. `doppler run -- cmd` / `infisical run -- cmd` inject values into one process's environment and nowhere else. "Integrations" sync values to Vercel, GitHub Actions, AWS and so on. |
| **1Password CLI** | References instead of values (`op://vault/item/field`). `op run` **masks secret values in the child's output** by default. `op inject` fills a template. Shell plugins ask for biometric approval per use. |
| **AWS Secrets Manager** | Rotation as a four-step machine (createSecret → setSecret → testSecret → finishSecret) with **staging labels**: `AWSPENDING`, `AWSCURRENT`, `AWSPREVIOUS`. Rollback means moving a label back. The "alternating users" strategy rotates database credentials without downtime. |
| **HashiCorp Vault** | **Dynamic secrets**: short-lived credentials minted per use, under a lease that expires. The best rotation is not having a long-lived key at all. |
| **GitHub Actions OIDC** | The same idea for CI: the workflow trades an OIDC token for short-lived cloud credentials, so no cloud key is stored in GitHub. |
| **SOPS / dotenvx** | Encrypted secret files that can be committed. We don't take this (§15), but it is a valid answer for teams that want values in git. |
| **GitHub push protection / gitleaks / trufflehog** | Provider token formats (`ghp_`, `sk_live_`, `AKIA…`, `xoxb-`) let a scanner recognize a leak, and they also identify **which provider** a key belongs to, so the scanner knows how to rotate it and can tell a test key from a live one. |

What maiTerm adds that none of them have is **the agent in the loop**. The agent does the
legwork (declares what the code needs, deploys, restarts, verifies, runs the rotation
checklist). The human makes the few irreversible decisions, on cards built from maiTerm's
own state.

## 3. Shape

```
Secret project  (bound to a repo root, docs/workspace-share.md's identity: remote URL + subpath)
├── environments   development · staging · production · custom (each has a TIER: dev | staging | prod)
├── keys           the names the code needs (declared by agents or the human)
│   └── per environment: versions labelled current / pending / previous
├── targets        approved destinations: GitHub Actions, Railway, Cloudflare, a .env file, a stack service…
├── policy         per tier: what may happen without a card (§8)
└── ledger         every delivery: key, environment, version, destination, when, by which tab
```

A tab resolves to a project through its folder, the same way Files and Tasks find theirs. An
agent sees only the project its tab is in. Using another project's keys needs a card.

## 4. Data model

Metadata is not secret and lives in AppData, like the MCP proxy's catalog. **Values live only
in the vault file (§5).** Kinds and labels are **strings, not serde enum variants**: a new
variant makes an older build load empty state (memory: "New serde enum variant = downgrade
wipe").

### 4.1 Project and environments

```rust
SecretProject {
    id, name,
    root: RepoRoot,                 // remote URL + subpath; a local-only folder uses its path
    environments: Vec<SecretEnv>,
    keys: Vec<KeyDef>,
    targets: Vec<SecretTarget>,
    policy: TierPolicies,
}
SecretEnv {
    id, name,                       // "development", "staging", "production", "preview-alice"
    tier: String,                   // "dev" | "staging" | "prod": policy follows the tier, never the name
    inherits_from: Option<String>,  // a preview env falls back to staging for keys it doesn't set
}
```

Three environments are created by default. **Tier, not name, decides policy**, so a custom
"prod-eu" environment is prod-tier and gets prod's guards.

### 4.2 Key definitions

```rust
KeyDef {
    name,                           // "STRIPE_SECRET_KEY": the env var name, used everywhere
    description: Option<String>,    // the AGENT'S words; always shown quoted, as agent text
    provider: Option<String>,       // "stripe" (declared, or detected from the value's format)
    format: Option<String>,         // a prefix or regex the value must match: "sk_(test|live)_"
    required_in: Vec<String>,       // tiers or env ids that must have a value
    generate: Option<GenerateSpec>, // { bytes: 32, encoding: "base64url" }: Rust may mint it
    template: Option<String>,       // "postgres://{{DB_USER}}:{{DB_PASS}}@db:5432/app" (§7.6)
    rotation: RotationPolicy,       // max_age_days, overlap, how to mint and revoke (§9)
    declared_by: Option<Origin>,    // tab + agent + time, for the panel's "asked for by"
}
```

### 4.3 Value metadata

```rust
ValueMeta {                         // plaintext metadata; the value itself is in the vault file
    key, env, version: u32,
    label: String,                  // "current" | "pending" | "previous"
    created_at, source: String,     // "human" | "generated" | "captured" | "adopted"
    expires_at: Option<i64>,        // when the provider says it dies (a GitHub PAT, say)
}
```

**Metadata holds no hash or fingerprint of a value.** A hash of a low-entropy secret (a
database password) can be brute-forced, and nothing in the design needs one outside Rust's
memory (§10.2).

### 4.4 Targets

```rust
SecretTarget {
    id, kind: String,               // "github_actions" | "railway" | "cloudflare_worker" | "vercel"
                                    // | "fly" | "env_file" | "stack_service" | "ssh_env" | …
    address: Map<String, String>,   // { repo: "maisoft/maiterm", environment: "production" }
    env: String,                    // which vault environment feeds it, exactly one
    keys: Vec<String>,              // which keys may go there; "*" is not allowed for prod tier
    approved: Approval,             // card id, by "desktop" | "phone", when
}
```

**A target takes exactly one environment.** That is the structural guard against the classic
mistake, a dev key in prod or a prod key on a laptop: there is no call that sends a staging
value to a target bound to production.

### 4.5 The ledger

Append-only, one row per delivery: `{key, env, version, target | file | service | run, at,
tab, agent}`. It is the consumer list for rotation (§9.2) and the audit log (§11.4). It
records what was delivered, not what happened after: a key the human copied into a dashboard
by hand is invisible to it, which is why rotation also searches for consumers (§9.2).

## 5. Storage, across macOS, Windows and Linux

> **Envelope encryption: ONE data key in the OS keychain, every value in one encrypted file.**

`keyring` v4 (already a dependency) covers all three platforms: the Apple Keychain, Windows
Credential Manager and Secret Service over zbus. Putting each secret in its own keychain item
looks simpler and is worse on every platform:

- **Windows caps a credential blob at 2,560 bytes** (`CRED_MAX_CREDENTIAL_BLOB_SIZE`). A PEM
  key or a GCP service-account JSON does not fit.
- **macOS asks for keychain access per item.** A dev build's ad-hoc signature changes on every
  rebuild and invalidates each item's ACL (see `map_err` in `accounts/vault.rs`), so fifty secrets
  would mean fifty prompts after a rebuild.
- **Secret Service** collections lock independently, and enumerating them is slow and differs
  by provider (GNOME Keyring, KWallet, KeePassXC).

So:

- **The data key**: 256 random bits under service `{app_data_slug()}.secrets`, user `vault-dek`.
  Dev and prod get separate entries, the same split as `accounts/vault.rs` rule 3.
- **The vault file**: `secrets.vault` in the data dir. A versioned header, then one record per
  value, sealed with **XChaCha20-Poly1305**. Each record's associated data is
  `project|env|key|version`, so a record can't be cut out and moved under another name. The
  file is written atomically (temp file, then rename) and is mode 0600 on Unix.
- **Headless Linux, WSL, a box with no Secret Service**: the same rule as `accounts/vault.rs`,
  **no plaintext fallback**. The human can choose a passphrase unlock instead: an
  Argon2id-derived key-encryption key, held in memory for the session, never written. Without
  one the vault is unavailable and says so.
- **In memory**: values are decrypted only for a delivery and held in a `Secret` type with a
  hand-written redacted `Debug`, `zeroize`d on drop. The existing `Secret` grows into this.
- **Crates**: `chacha20poly1305`, `argon2`, `zeroize` (RustCrypto, pure Rust, all three
  platforms). No OpenSSL.

**Backups.** A backup that includes `secrets.vault` is useless on another machine, because the
data key stays in this machine's keychain. That is intended. Moving the vault is an explicit
**Export vault** with a passphrase, in age's format so the file opens with the `age` CLI too.

## 6. The agent's side: MCP tools

Every tool here is on `OWN_TAB_ONLY_TOOLS` and `PEER_ADDRESSING_TOOLS`
(`claude_code/server.rs`): it acts for the calling tab on a stated identity, and the cards
name that tab as the agent asking. Tools are listed only while the vault is enabled, like the
browser-link tools.

| Tool | Acts at once? | What it does |
|---|---|---|
| `listSecrets` | yes: read-only | The project's keys × environments: set / missing / pending / expiring / overdue, age, provider, targets, the latest delivery per target. **Never a value, a length, or a fingerprint.** |
| `declareSecrets` | yes | Adds key definitions (§4.2): names, descriptions, format, `required_in`, `generate`. Names are harmless, and nothing is sent anywhere. Changing a definition the human set needs a card. |
| `declareTargets` | **never: a request** | Proposes destinations (§4.4). Widens where values may go, so the human approves. maiTerm checks each address with its adapter before the card is filed (the repo exists, the Railway service exists). |
| `requestSecretValues` | **never: a request** | Opens the entry sheet (§6.1) for named keys × environments. Settles when the human clicks Done. |
| `generateSecret` | dev tier: yes, if the value is missing. Otherwise a request | Rust mints a value from the key's `GenerateSpec`. Replacing an existing value is a rotation (§9), never a generate. |
| `deploySecrets` | per tier policy (§8) | Pushes keys from one environment to one approved target. The reply lists which keys and versions went. |
| `runWithSecrets` | per tier policy (§8) | Runs a command with keys in its environment (§7.1). |
| `writeEnvFile` | dev tier, an approved `env_file` target: yes | Renders a `.env` from the vault (§7.4). |
| `adoptSecrets` | **never: a request** | Rust reads an existing `.env` (or a JSON/YAML file plus key paths), stores the values, and on approval rewrites the file's lines as references. The agent organizes the cleanup and never reads a value. |
| `captureSecret` | **never: a request** | Runs a mint command (`stripe keys create …`, `openssl rand -hex 32`) and stores its output (or a JSON path into it) as a pending value, without returning it to the agent (§9.4). |
| `requestRotation` | **never: a request** | "The agent has requested to rotate these keys…" (§9.3). |
| `getRotation` / `reportRotationStep` | yes | Reads a rotation run, or records the agent's result for one of its steps (§9.1). |

### 6.1 The entry sheet, and "Done"

This is the flow Darryl described:

1. The agent calls `declareSecrets` with the names the code needs, `declareTargets` with where
   they go, then `requestSecretValues({keys, envs, reason})`. The three can be combined: any
   undeclared name passed to `requestSecretValues` is declared first.
2. maiTerm files an agent request (docs/agent-requests.md, new kind `secret_values`). Its card
   in the Loom's Decisions, and its corner card on the agent's terminal, reads something like
   **"Secrets Vault feature asks you to set 4 values in maiterm: STRIPE_SECRET_KEY
   (development, production), RESEND_API_KEY (development)…"**, with the agent's `reason`
   quoted. The button is **Enter values**, not Approve.
3. The sheet opens. One row per key and environment:
   - the name and environment, and the agent's description shown **as a quote**;
   - the provider (declared, or detected from what is pasted) with a link to where that
     provider issues keys;
   - a masked input (`autocomplete="off"`, `spellcheck="false"`, `autocapitalize="off"`, and no
     smart-quote substitution: memory "Smart quotes fix");
   - **checks as the human types**, all in Rust: the format matches; **a test-mode key in a
     prod-tier environment, or a live key in dev, is flagged** (`sk_test_` in production); the
     same value in two tiers is flagged; leading and trailing whitespace is trimmed (the
     `Secret::new` lesson);
   - **Generate** when the key has a `GenerateSpec`, and **Skip**.
4. **Done** sends each value to Rust once (one `invoke` per value, never kept in a store, the
   field cleared) and settles the request with a per-key outcome. **Cancel** denies it.
5. The agent is told through the existing settle path (`agent-request-settled` →
   `overlordStore.tellTab`, which never types over a draft), framed as
   `⟦SECRETS⟧ Values set: STRIPE_SECRET_KEY (development, production). Skipped:
   RESEND_API_KEY (development).` From there it carries on: `deploySecrets`, `runWithSecrets`.

**Request lifetime.** Agent requests expire after 30 minutes. Typing values can take longer
(finding the right dashboard, waiting on a colleague), so `secret_values` gets 24 hours.
Requests are in memory: after a restart the agent's `getRequestStatus` says it is gone, and
the agent asks again.

**The webview holds a value for an instant.** It is the human's own machine, and the
alternative, a native dialog per platform, costs three implementations. The rules: never in a
Svelte store, never logged (dev builds attach the console), never in an error message, and
the input element is cleared on submit and on unmount.

## 7. Delivery: getting a value where it needs to be

Every delivery path follows the same three rules:
- **stdin or an API, never argv.** A test runs every adapter against a fake binary and fails if
  any value shows up in argv.
- **Never typed into a PTY.**
- **Recorded in the ledger.**

### 7.1 `maiterm run`: a process with secrets in its environment

The equivalent of `doppler run`. The maiTerm binary already takes a different path on certain
argv (`run_browser_link_host_if_requested` in `main.rs`), and it gets one more:

```
maiterm run --ticket <t> -- npm run dev
```

- **The ticket** comes from `runWithSecrets` (or a stack service start, §7.2). It is single-use,
  valid for 60 seconds, and bound to the command line, the folder and the key set the human
  approved. The shim gives the ticket to maiTerm along with its own argv and cwd. maiTerm
  checks they match, then returns the values over the local connection, never through a file.
  A ticket in scrollback is spent the moment it is redeemed.
- **On Unix** the shim `exec`s the command, so the process tree is exactly what it would be
  without it. **On Windows** there is no `exec`: the shim spawns the child, forwards
  Ctrl-C/Ctrl-Break and returns the child's exit code.
- **Output masking** (1Password's idea). When stdout/stderr is not a TTY, the shim stays as
  the parent, pipes the output and replaces any value it holds, plus its base64, URL-encoded
  and hex forms, with `«STRIPE_SECRET_KEY»`. An agent running a command reads the output, so
  this matters. On a TTY (an interactive dev server) piping would break the terminal, so
  masking is off and the card says so.
- **Agent mode** (`runWithSecrets({command, keys, env, wait: true})`): maiTerm runs the command
  itself, in its own process group as watch scripts are (`watch.rs`), and returns the masked
  output as the tool result. That is the right default for one-shot jobs such as migrations or
  seed scripts.

**What a `run` approval does NOT cover, said plainly on the card:** the approval binds the
command line, not the code it runs. An agent that can edit `scripts/seed.js` can make the
approved `node scripts/seed.js` print `process.env`. Local execution with a secret means
trusting the code with it, which is why prod-tier keys don't run locally by default (§8).

### 7.2 Stack services

Today a service starts by typing `cd`, `export`s and the command into its shell
(docs/stack.md §4). **A secret cannot be delivered that way**: it would land in the
scrollback, the shell history and the serialized scrollback in `aiterm-state.json`. So:

- `Service` gains `secrets: Vec<{env_var, key}>` and `secret_env: Option<String>` (which vault
  environment).
- When a service has secrets, its start types `maiterm run --ticket <t> -- <command>`, with a
  fresh ticket each start and restart. The plain `env` exports stay as they are for
  non-secret values.
- `restartService` after a `deploySecrets` to a `stack_service` target is how an agent picks up
  a new value. A rotation does this for every service in its ledger (§9.2).
- The shell underneath never holds the values. Only the service process does, so a Ctrl-C in
  the console leaves nothing behind.

### 7.3 Deploy targets (adapters)

Each `kind` is an adapter in `src-tauri/src/secrets/targets/` with five operations: `check`
(the address exists and maiTerm can reach it), `push`, `list` (which names are there and when
they changed; providers never return values), `remove`, and `verify`. Initial set, each to be
checked against the real CLI or API before it ships:

| Target | How the value travels |
|---|---|
| GitHub Actions (repo, environment, org) | `gh secret set NAME [--env E]` reading **stdin** |
| Cloudflare Workers | `wrangler secret put NAME` reading **stdin** |
| Vercel | `vercel env add NAME <environment>` reading **stdin** |
| Fly.io | `fly secrets import` reading `NAME=value` lines on **stdin** |
| Railway | the GraphQL API (`variableUpsert`). The CLI's `--set K=V` puts the value in **argv**, so it is not used |
| AWS Secrets Manager / SSM | the API over the SDK. The CLI's `--secret-string` is argv |
| Kubernetes | a Secret manifest on **stdin** to `kubectl apply -f -` |
| `env_file`, `stack_service`, `ssh_env` | local, see §7.2, §7.4, §7.5 |

**A platform's own credential (a Railway token, a Cloudflare API token) is itself a vault key**,
marked `deployer`. Deployer keys are never delivered anywhere except to their own adapter.
Where the human is already signed in to the CLI (`gh auth`), the adapter uses that and stores
nothing.

**OIDC advisor.** When a GitHub Actions target holds a cloud key (`AKIA…`, a GCP
service-account JSON), the panel suggests federating with OIDC and dropping the key. It is a
suggestion, nothing is changed.

### 7.4 `.env` files

The weakest destination, and the one every framework reads. Rules:
- dev tier only, unless the human enables it per target with a card that says "a production
  value will be on disk in plain text";
- the path must be inside the project and **ignored by git** (`git check-ignore`), checked
  every write;
- mode 0600, written atomically;
- the file gets a header naming maiTerm as the writer, and the ledger knows the file, so a
  rotation rewrites it.

The panel nudges toward `maiterm run`, which leaves nothing on disk.

### 7.5 SSH hosts

For an ssh tab, the place is `user@host:folder`, the same identity watch scripts use
(`watch::place`). Values go **over the stdin of maiTerm's own ssh connection**, through the
tunnel's ControlMaster socket, as `watch/remote.rs` does and as the setup-token handoff does
(docs/login.md §6). `runWithSecrets` on an ssh tab runs a stdin-fed wrapper on the host that
reads the environment, `exec`s the command, and is killed as a group afterwards. Nothing is
written to the remote disk unless the target is an `env_file` there. The card says **which
host**.

### 7.6 Templates and references

A key may be a template over other keys (`DATABASE_URL =
postgres://{{DB_USER}}:{{DB_PASS}}@db:5432/app`). The agent writes the template, which holds
no secrets, and Rust fills it in at delivery. Rotating `DB_PASS` re-renders `DATABASE_URL`
everywhere it went. The same syntax works in any file a target renders.

## 8. Environments and policy

Policy goes by tier. These are the defaults, and the human can change them per project in the
panel. **No MCP tool can change policy**, and policy is not in `preference_meta`.

| | dev | staging | prod |
|---|---|---|---|
| `deploySecrets` to an approved target | at once | at once | **a card each time**, listing keys and version changes (`STRIPE_SECRET_KEY v3 → v4`) |
| `runWithSecrets` | a card once per command + place, then at once | same as dev | **off**. The human can turn it on, then it is a card per run |
| `writeEnvFile` | at once to an approved file | a card | off |
| `generateSecret` (missing value) | at once | at once | a card |
| reveal / copy in the panel | yes | yes | after an OS user-presence check (§16, a later phase) |
| leak found | reported | reported, rotation proposed | reported, **rotation proposed as a Decision** |

A prod card can be approved on the phone (§12): it names what moves and where, never a value.

## 9. Rotation

### 9.1 A rotation run

A run rotates one or more keys in one environment as a single plan. It follows the AWS
staging-label model:

```
planned → minting → staged (pending) → distributing → verifying → promoted → overlap → revoking → done
                                    ↘ any failure before revoking: roll back (previous → current, redeploy) ↙
```

1. **Plan.** For each key: its consumers (the ledger plus discovered ones, §9.2), how to mint
   (an adapter, a capture recipe (§9.4), or the human), whether the provider allows two live
   keys at once, and how to revoke.
2. **Mint** → the new value is stored as `pending`.
3. **Distribute** the pending value to every consumer: push to each target, rewrite each
   `.env`, restart each stack service.
4. **Verify.** The adapter's `verify`, the provider's own check where there is one (a whoami
   call with the new key), and **the agent's checks**: it runs tests, hits health endpoints
   and reports each through `reportRotationStep`. A watch script (docs/follow-ups.md §5.1)
   can wait on "the deploy is live" without spending a turn.
5. **Promote**: pending becomes current, and the old current becomes previous.
6. **Overlap.** The old key stays live for a window (default 24 h, or until the human says
   so), so caches, workers and a colleague's laptop can catch up.
7. **Revoke** the old key: an adapter call, or a card for the human ("I've revoked it in the
   Stripe dashboard"). Previous values are purged after the retention period (default 7
   days).

Before revoking, **rollback is cheap**: move the labels back and redeploy previous to every
consumer that already switched. After revoking it is impossible, which is why revoking is
always a human step unless an adapter does it *and* the human ticked "revoke automatically"
on that run's card.

**Single-key providers** (some database passwords: a new one kills the old) have no overlap.
The card says so ("there will be a moment where the app has the wrong password"), and offers
the **alternating users** strategy where the provider supports it: two database users, each
rotation switching the app to the other one and resetting the idle user's password.

**The run is a task workstream** ("Rotate production keys, 22 Oct"), so the board and the
phone show each step, and the agent works through it with the ordinary task tools.

### 9.2 Finding every consumer

- **The ledger**: everywhere maiTerm delivered.
- **Discovery.** The agent searches the repo and its CI config for the key's *name*
  (`STRIPE_SECRET_KEY` in a workflow file, `process.env.X`). Rust scans for the *value* in
  files under the project root and in the human's other known projects, and reports "found in
  ~/old-project/.env" without showing what it found.
- **Unknown consumers** (a teammate's laptop, a manual dashboard entry) can't be found. The
  card lists what *is* known and says "and anywhere you copied it by hand". The overlap
  window exists for exactly that.

### 9.3 "The agent has requested to rotate these keys"

`requestRotation({keys, env, reason})` files a `secret_rotation` request. The card is
maiTerm's words, built from the ledger:

> **Secrets Vault feature asks to rotate 3 keys in production**
> STRIPE_SECRET_KEY · SENDGRID_API_KEY · DB_PASS
> They are used in 5 places: GitHub Actions (maisoft/api, production), Railway (api,
> production), Railway (worker, production), stack service `api`, and DATABASE_URL (a template).
> Minting: Stripe and SendGrid by you, DB_PASS generated. Revoking: by you, after a 24 h overlap.
> DB_PASS has no overlap: expect a few seconds of failed connections.
> *"Rotating after the contractor offboarding today"* (the agent)
> **[Start rotation]** [Not now]

On **Start rotation** the run begins. When a step needs the human (pasting a newly minted key,
confirming a revocation), maiTerm files another card on the same run, and the agent is told
when it is answered, just as with `requestSecretValues`.

**maiTerm proposes rotations itself**, through the same card, with no agent involved:
- a key past its `max_age_days`;
- a key whose provider expiry is close;
- a leak (§10.2).

### 9.4 Capture recipes: rotation without an adapter for every provider

Most providers have a CLI or API that mints keys, and the value comes back on stdout. That is
exactly where an agent must not see it. `captureSecret({key, env, command, extract})` runs the
command through the same runner as §7.1 and **stores** the output instead of returning it:
- `extract` is a JSON path or a regex;
- the agent gets back "captured, 51 characters, matches the Stripe format".

Approval is keyed by command + place, like a watch script. A revoke command works the same
way, with the old value going *in* through stdin or a template. Recipes that work can be saved
on the key's `RotationPolicy`, so the next rotation of that key needs only the start card.

### 9.5 Provider packs

A small built-in table per provider: format prefixes (test vs live), the console URL for
issuing and revoking keys, whether two keys can be live at once, typical expiry, and which
adapter or recipe mints and revokes. Initial candidates: Stripe, GitHub, AWS, Anthropic,
OpenAI, Slack, SendGrid, Resend, Cloudflare, Supabase. **Every entry must be checked against
the provider's current docs when it is written**, not from memory.

## 10. Keeping secrets out of sessions

### 10.1 What maiTerm can and can't stop

- **Where maiTerm owns the store, it scrubs**: serialized scrollback, its logs, toasts, the
  phone mirror and share files are checked for live values and redacted before they are
  written.
- **Where someone else owns the store, maiTerm reports.** Claude Code's transcript is Claude
  Code's file, and **anything in it has already been sent to the model provider**. Editing the
  file does not undo that. So a value found there is treated as leaked, and the response is
  rotation, not cleanup.
- **It can't stop an agent's own tools from reading a file.** If `.env` holds a value, the
  agent's Bash can `cat` it. The mitigations below make that rare. They don't make it
  impossible.

### 10.2 Leak detection

Rust holds the current, pending and previous values in memory while the vault is unlocked and
runs one Aho-Corasick pass (raw, base64, URL-encoded, hex) over:
- Claude JSONL and Codex rollouts as maiLink already tails them (`mailink/transcript.rs`);
- the terminal grid at scrollback-serialize time;
- the git diff of a commit, if the human enables the optional pre-commit hook;
- files under project roots, when asked during discovery (§9.2).

A hit reports **which key, which environment, where and when**, never the surrounding text,
and per §8 raises a rotation proposal. Short values (under 12 characters) are skipped, since
they would match by accident. The panel notes this.

### 10.3 Making the wrong thing hard

- `adoptSecrets` turns an existing `.env` into references, so there is less on disk to read.
- The hook maiTerm already receives (`PreToolUse`, `claude_code/gate.rs`) can deny a Bash or
  Read call that targets a file the ledger says holds values, with a reason pointing the agent
  at `runWithSecrets`. **To verify:** that a deciding `PreToolUse` from maiTerm's hook works
  on every runtime we support (Codex hook parity is listed in docs/codex-integration-review.md).
- The priming line (`initSession`) tells agents the vault exists and which tools to use, so
  "paste your API key here" is not the agent's first idea.

## 11. UI

### 11.1 The Secrets panel

A window-level view per project (opened from the workspace's context menu and a sidebar row;
the shortcut is an open question):
- **a matrix of keys × environments**, each cell showing set (with age), missing (red when
  required), pending (a rotation in progress), overdue, or expiring;
- expanding a key shows its description (quoted as the agent's), provider, template, targets,
  version history (no values), consumers from the ledger, and its audit rows;
- actions: Set, Generate, Rotate, Remove from target, Reveal, Copy.

**Reveal** shows the value for 10 seconds. **Copy** clears the clipboard after 30 seconds if
it still holds that value. Both are written to the audit log. Values are masked by default,
because screens get shared.

### 11.2 Cards

New agent-request kinds, each rendered by `AgentRequestCard` with the existing guards (no
decision in a card's first 1.5 s, the second click of a double-click ignored):
`secret_values`, `secret_targets`, `secret_deploy` (prod), `secret_run`, `secret_adopt`,
`secret_capture`, `secret_rotation`, plus the rotation steps that need the human. As with
every card: **the words are maiTerm's, the agent's only as a quoted `reason`; they must be
true about what runs and where; and they never carry a value**, since cards reach the phone.

### 11.3 Rotation progress

The run's workstream on the board, plus a run view in the panel: keys down the side,
consumers across, each cell pending / pushed / verified / failed, with the overlap countdown
and the revoke button.

### 11.4 Audit log

Append-only in the data dir: every value set, generated, revealed, copied, delivered,
rotated or revoked, with who (tab, agent, runtime, account), what (key, environment,
version), where, and approved by (card, phone, policy). It never records a value. It can be
filtered in the panel and exported.

## 12. The phone

- **Approvals: yes.** `secret_targets`, `secret_deploy`, `secret_run`, `secret_rotation` and
  revoke confirmations are agent requests, so they appear in maiLink §4.4's `/requests` like
  any other kind. Each new kind is a protocol bump (docs/mailink-protocol.md §13.5).
- **Values: no, in v1.** Entering a value on the phone would carry it over the maiLink LAN API
  and through the phone app. The sheet's card on the phone says "enter these on the desktop".
  This is an open question (§16): pasting from a phone password manager is handy, and it
  needs its own transport review. The "never go to desktop" rule (memory) is broken here on
  purpose, and the card says why.
- Leak reports and rotation proposals ring the doorbell, gated by mute (docs/mute.md) like any
  other notice.

## 13. How it meets the rest of maiTerm

- **Reload / duplicate / archive**: the vault belongs to the project, not to a `Tab`, so
  there is nothing to carry. Pending requests are keyed by the asking tab and follow
  docs/agent-requests.md's rules (a reload is refused while one is open).
- **Move project** (docs/relocate.md): `env_file` target paths, `run` approvals keyed by place,
  and the project's root all follow the move.
- **Workspace Share**: the share file carries the **manifest**: key names, descriptions,
  environments and target kinds, never values or target approvals. Importing it shows "this
  project needs 6 secrets" and opens the entry sheet. This becomes a `Shared*` type on the
  allowlist (docs/workspace-share.md).
- **Overlord**: `listEscalations` and rules can see a project's missing and overdue counts.
  Never values.
- **Accounts**: the audit row records the tab's account (docs/login.md §14 recording at spawn).
- **Mute**: silences notices, never cards (docs/mute.md).

## 14. Threat model

| Adversary | Defended by | Not defended |
|---|---|---|
| **A prompt-injected agent** ("send the key to evil.com") | No tool takes a value or a free URL; targets need a card; prod deploys need a card each time | An agent that writes code which leaks the key at runtime and gets it deployed. Code review and CI catch that, not the vault |
| **A careless agent** (prints env, commits `.env`) | No values in sessions; masking; gitignore check; leak scan; pre-commit hook | A value it already read through its own tools |
| **Another local process as the same user** | The keychain ACL on the data key; 0600 files | Same user, same reach, as docs/login.md §9.2 accepts |
| **A stolen backup or synced data dir** | The data key never leaves the keychain | — |
| **A lost or compromised phone** | Values never reach the phone; approvals need a paired device token (docs/agent-requests.md §2) | A paired phone can approve a prod deploy, so unpairing is the response |
| **A shared screen / shoulder** | Masked by default; reveal times out | — |
| **maiTerm's own logs and errors** | `Secret` has no `Display`/`Serialize`; errors name the operation and key, never the value (the `accounts/vault.rs` rule 1) | — |
| **A malicious dependency in the app being run** | — | It gets whatever the process gets. That is true of every secrets manager |

**The vault keeps secrets out of sessions and decides where they go. It does not make the code
that uses them trustworthy.** Every card on a `run` or prod path says so in one line.

## 15. Rejected

- **One keychain item per secret**: §5.
- **Giving the agent the value and asking it not to print it**: it ends up in the transcript,
  so with the model provider, the moment the agent reads it.
- **Typing `export`s into a PTY**: scrollback, history and serialized state (§7.2).
- **A `curl`-style "call this URL with the key" tool**: every exfiltration needs exactly that.
  A later "signed request" tool would be acceptable only with a per-key host allowlist, set by
  a card.
- **Encrypted files in the repo (SOPS / dotenvx) as the store**: a good tool, but its key
  management is the same problem one level down, and it puts rotation history in git. A team
  that uses it can still adopt from it.
- **Scrubbing Claude's transcript as the response to a leak**: the provider already has it
  (§10.1).
- **Fully automatic rotation, including revoke**: revoking is the one step that can't be
  undone, so it stays with the human unless they tick otherwise on that run's card.

## 16. Open questions

1. **Phone value entry**: worth a transport review for v1, or later?
2. **Prod reveal/copy**: an OS user-presence check (Touch ID through LocalAuthentication,
   Windows Hello through `UserConsentVerifier`, polkit on Linux) is three implementations. Is
   it v1, or does prod simply have no reveal until it exists?
3. **Bring your own backend**: let maiTerm keep the manifest, targets, ledger and rotation
   runs while the values live in 1Password / Doppler / Infisical (resolved through their CLIs
   at delivery, written back on rotation). That is the path to teams sharing secrets, which v1
   doesn't do. How soon?
4. **Which targets and providers first?** The tables in §7.3 and §9.5 are candidates.
5. **Default prod `run` policy**: off (as above), or a card per run?
6. **The `PreToolUse` deny (§10.3)**: is it enough, or should maiTerm also write `Read(.env)`
   deny rules into the project's Claude settings? That writes user config, so reviewers need a
   scratch HOME (memory "Reviewers must isolate HOME").
7. **Panel placement and shortcut.**
8. **Dynamic secrets**: mint short-lived credentials per `run` where the provider supports it
   (AWS STS, database IAM auth). A real step past rotation. Which phase?

## 17. Build order

1. **Vault core.** `src-tauri/src/secrets/`: the envelope store across platforms (§5),
   passphrase unlock, the data model, the Secrets panel with human entry, environments, the
   audit log. `accounts/vault.rs`'s `Secret` grows `zeroize` and moves to a shared module.
2. **The agent manifest.** `listSecrets`, `declareSecrets`, `requestSecretValues`, the entry
   sheet, the ⟦SECRETS⟧ notice, `generateSecret`. **This is the first phase an agent can use,
   and it proves Darryl's flow end to end.**
3. **Local delivery.** The `maiterm run` shim with tickets and masking, `runWithSecrets`, stack
   service secrets (§7.2), `writeEnvFile`, `adoptSecrets`, scrollback redaction.
4. **Targets.** `declareTargets`, `deploySecrets`, the ledger, adapters for GitHub Actions and
   one host (Railway or Cloudflare) first.
5. **Rotation.** Runs with human mint and revoke, `requestRotation`, the run view, age and
   expiry proposals, provider packs.
6. **Leak detection.** Transcript, scrollback and pre-commit scans, leak-triggered rotation.
7. **Automation and reach.** Capture recipes, adapter mint/revoke, ssh delivery, bring-your-own
   backend, OS user-presence for prod.

Each phase is reviewed before the next. The shim and the adapters must be reviewed with a
scratch HOME, and **the "no value in argv" test must exist before the first adapter ships.**
