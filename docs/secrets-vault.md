# Secrets Vault: keys agents can use but never see

2026-10-22. Design only, nothing is built yet. Darryl: "It's important to keep secrets out of
sessions, yet it's very handy for agents to be able to pipe them where they need to be." Also
on the list: key rotation, "a fairly intensive process" for developers, and dev/prod key sets.
The shape Darryl proposed is the spine of this design. An agent declares the key **names** it
needs and the **platforms** it may deploy to, pushes up a UI where the human types the values,
and is told when the human clicks Done. An agent can also ask to rotate keys: "The agent has
requested to rotate these keys: a, b, c. Do you want to continue?"

**Reviewed the same day** (a review of d2a441c8). It found eight ways an agent could still
reach a value through the first draft, and they are fixed below. The most important result is
§1's second paragraph: what the vault can promise against an agent running as the same OS
user.

## 1. The rule

> **An agent handles a secret's name, never its value. Rust looks up the value at the moment
> it is used, and sends it to a place the human approved.**

**What this protects against, and what it doesn't.** An agent's Bash tool is a process
running as the human's own OS user. Such a process can read:
- the keychain item (Linux Secret Service and Windows Credential Manager have no per-app ACL,
  so any process can read an unlocked item without a prompt);
- another process's environment (`ps eww`, `/proc/<pid>/environ`);
- any file the human can read.

So the vault stops **accidental and naive exposure** (an agent printing env, pasting a key
into chat, committing `.env`). It also stops **a prompt-injected agent sending a value
somewhere directly**. It does **not** stop a same-user agent that is set on getting a value
and knows where to look. For prod keys, the real boundary is running agents sandboxed (Claude
Code's sandbox) or as a different OS user, and §5 puts prod values behind an unlock the OS
enforces (how far that holds differs per platform; §5 says exactly). Every card on a prod path says this in one line.

Everything else follows from the rule:

1. **No MCP tool returns a value, and no tool accepts one.** Values reach the vault in five ways
   only: the human types them into maiTerm's own entry sheet, Rust generates them, Rust
   captures them from an approved command's output, Rust reads them out of a file (§6), or the
   human imports a vault export (§5).
2. **A value never passes through a PTY, argv, a log line, `aiterm-state.json` or the scrollback
   database, the phone, a share file, or a tool result.** The remote setup-token kept itself out
   of argv, the terminal and state the same way (docs/login.md §9). The analogy stops there: that
   token lives in the remote shell's environment, which this design avoids where it can (§7.5).
3. **Secrets are tied to destinations.** Each key and environment lists where it may go. A
   prompt-injected agent's natural move is "send STRIPE_KEY to this URL". The defense is that
   no tool takes an arbitrary URL, and that **an agent's files and PATH cannot change where an
   approved delivery goes** (§7.3). Masking output is a backstop, not the protection.
4. **Nothing an agent controls can answer questions about a value.** A length, a "matches",
   or a leak hit on text the agent wrote are each one bit of the value per call (§9.4, §10.2).
5. **maiTerm knows every place it put a value.** Each delivery is recorded in a ledger (§4.5).
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
Secret project  (bound to local folders the human approved; the repo's remote URL only matches shares)
├── environments   development · staging · production · custom (each has a TIER: dev | staging | prod)
├── keys           the names the code needs (declared by agents or the human)
│   └── per environment: versions labelled current / pending / previous
├── targets        approved destinations: GitHub Actions, Railway, Cloudflare, a .env file…
├── policy         per tier: what may happen without a card (§8)
└── ledger         every delivery: key, environment, version, destination, when, by which tab
```

**A project is bound to canonical local paths the human approved**, not to a remote URL.
Anyone can `git init` a folder and add another project's remote. The remote URL (with the
subpath, docs/workspace-share.md's identity) is used only to match a project on share import.

A tab resolves to a project from **its shell's real working directory, read by Rust** (as
Files does), never from the OSC 7 cwd: any process in the tab can write an OSC 7 to the
terminal. An agent sees only the project its tab is in. Using another project's keys needs a
card.

## 4. Data model

Metadata is not secret and lives in AppData, like the MCP proxy's catalog. **Values live only
in the vault file (§5).** Kinds and labels are **strings, not serde enum variants**: a new
variant makes an older build load empty state (memory: "New serde enum variant = downgrade
wipe").

### 4.1 Project and environments

```rust
SecretProject {
    id, name,
    paths: Vec<String>,             // canonical local roots the human approved (§3)
    remote: Option<RepoRoot>,       // remote URL + subpath: share-import matching only
    environments: Vec<SecretEnv>,
    keys: Vec<KeyDef>,
    targets: Vec<SecretTarget>,
    policy: TierPolicies,
}
SecretEnv {
    id, name,                       // "development", "staging", "production", "preview-alice"
    tier: String,                   // "dev" | "staging" | "prod": policy follows the tier, never the name
    inherits_from: Option<String>,  // SAME TIER ONLY: a preview env falls back to development
}
```

Three environments are created by default. **Tier, not name, decides policy**, so a custom
"prod-eu" environment is prod-tier and gets prod's guards.

**Inheritance never crosses a tier.** Otherwise a prod-tier environment inheriting from
staging would ship staging values to production, and a dev environment inheriting from prod
would hand prod values to dev policy (`.env` writes and local runs at once).

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

**Two fields are locked once the key matters**, or they become a way around the rest:
- `format` is fixed once any value exists. An agent able to change it and re-validate would
  have an oracle on the value (§1 rule 4).
- `template`: any change on a key that has a value or a target binding needs a card, and the
  card shows the template text. Otherwise an agent turns a key it declared into
  `{{STRIPE_SECRET_KEY}}`, and the next at-once deploy sends Stripe's value to a target that
  was approved for something else (§4.4).

### 4.3 Value metadata

```rust
ValueMeta {                         // plaintext metadata; the value itself is in the vault file
    key, env, version: u32,
    label: String,                  // "current" | "pending" | "previous"
    created_at, source: String,     // "human" | "generated" | "captured" | "adopted" | "imported"
    expires_at: Option<i64>,        // when the provider says it dies (a GitHub PAT, say)
}
```

**Metadata holds no hash, fingerprint or length of a value.** A hash of a low-entropy secret (a
database password) can be brute-forced, and a length is a fingerprint (the `Secret` `Debug`
comment in `accounts/vault.rs` says the same).

### 4.4 Targets

```rust
SecretTarget {
    id, kind: String,               // "github_actions" | "railway" | "cloudflare_worker" | "vercel"
                                    // | "fly" | "aws_secrets" | "kubernetes" | "env_file" | "ssh_env"
    address: Map<String, String>,   // { repo: "maisoft/maiterm", environment: "production" }
    binary: Option<String>,         // the CLI's ABSOLUTE path, resolved when the target was approved (§7.3)
    env: String,                    // which vault environment feeds it, exactly one
    keys: Vec<String>,              // which keys may go there; "*" is not allowed for prod tier
    approved: Approval,             // card id, by "desktop" | "phone", when
}
```

**A target takes exactly one environment.** That is the structural guard against the classic
mistake, a dev key in prod or a prod key on a laptop: there is no call that sends a staging
value to a target bound to production. (Inheritance stays inside a tier, §4.1, so it can't
undo this.)

**`keys` is checked against the resolved set**: a templated key counts as every key its
template uses, so a template can't carry a value past the allowlist.

Stack services are **not** a target kind: a service's binding is on the service (§7.2).

### 4.5 The ledger

Append-only, one row per delivery: `{key, env, version, target | file | service | run, at,
tab, agent}`. It is the consumer list for rotation (§9.2) and the audit log (§11.4). It
records what was delivered, not what happened after: a key the human copied into a dashboard
by hand is invisible to it, which is why rotation also searches for consumers (§9.2).

## 5. Storage, across macOS, Windows and Linux

> **Envelope encryption: data keys in the OS keychain, every value in one encrypted file.**

`keyring` v4 (already a dependency) covers all three platforms: the Apple Keychain, Windows
Credential Manager and Secret Service over zbus. Putting each secret in its own keychain item
looks simpler and is worse on every platform:

- **Windows caps a credential blob at 2,560 bytes** (`CRED_MAX_CREDENTIAL_BLOB_SIZE`). A PEM
  key or a GCP service-account JSON does not fit.
- **macOS asks for keychain access per item.** A dev build's ad-hoc signature changes on every
  rebuild and invalidates each item's ACL (see `map_err` in `accounts/vault.rs`), so fifty
  secrets would mean fifty prompts after a rebuild.
- **Secret Service** collections lock independently, and enumerating them is slow and differs
  by provider (GNOME Keyring, KWallet, KeePassXC).

So:

- **Two data keys.** 256 random bits each, under service `{app_data_slug()}.secrets`:
  - `vault-dek` seals dev- and staging-tier values. It is the keychain item alone.
  - `vault-dek-prod` seals prod-tier values, and **the OS guards it**, not just a dialog (§16
    decision 2):
    - **macOS**: an item in the **data-protection keychain** (`kSecUseDataProtectionKeychain`),
      `AccessibleWhenUnlockedThisDeviceOnly` plus a user-presence access control. The keychain
      itself then refuses to release it without Touch ID or the login password, and the
      `security` CLI can't reach it at all (it reads only file-based keychains). Three costs:
      - The data-protection keychain needs an `application-identifier` /
        `keychain-access-groups` entitlement, so **a Developer ID provisioning profile embedded
        in the release build**. maiTerm has neither today; that is release-pipeline work in
        phase 3.
      - **Ad-hoc dev builds can't use it** (errSecMissingEntitlement) and use the passphrase.
      - `keyring`'s `protected` feature can require user presence, but pairs it with
        `AccessibleWhenUnlocked`, not ThisDeviceOnly, which would let the item travel in a
        backup. So this one item uses `security-framework` directly.
    - **Windows**: a Windows Hello key (`KeyCredentialManager`) signs a fixed per-install
      challenge, and HKDF over that signature (with a random per-install salt) is the key that
      unwraps `vault-dek-prod`. Bitwarden desktop uses the same pattern. The rules:
      - Enrollment signs twice and compares, so a change to randomized padding fails loudly
        instead of locking the vault.
      - The signature is the key: it is never persisted or logged.

      Two weaknesses to state on the card that turns prod on:
      - **For an unpackaged app the Hello credential is not app-bound.** A same-user agent can
        open the same credential and raise a Hello prompt that doesn't clearly name who is
        asking. So: **refuse any Hello prompt maiTerm didn't just ask for.**
      - **Windows has no hardened runtime.** While unlocked, any same-user process can read
        maiTerm's memory (`ReadProcessMemory`), the unwrapped key included.

      Not `UserConsentVerifier`, which only shows a prompt and releases no key material.
    - **Linux, and dev builds anywhere**: a passphrase (Argon2id).

    **Unlocked until 15 minutes idle, where idle means no human-initiated use**: an unlock, a
    reveal or copy, a prod delivery approved by a click. Agent calls, the scanner and the
    scrubber never count, so an agent can't hold it open.

    **Phase 1 → phase 3 migration.** Phase 1 wraps the prod key with a passphrase everywhere.
    When the OS guard becomes available, phase 3 **re-wraps the key under it and deletes the
    passphrase wrap**. Otherwise a same-user agent can attack the passphrase blob offline, or
    show a fake passphrase prompt, and the OS guard means nothing.

    **Opting dev and staging into the guard is per project**: that project's dev and staging
    values are then sealed under `vault-dek-prod` instead of `vault-dek`. Their "at once"
    actions in §8 then need the unlock too, and wait for it like prod ones.

    **What the lock does not cover: detection.** The scrubber (§10.1) and the scanner (§10.2)
    need the values they look for. A service started while unlocked keeps printing after the
    lock. So from the first prod unlock after launch, prod match tables stay in memory until
    maiTerm quits; the lock governs **use** (delivery, reveal, export), not detection. The cost,
    stated: on Windows those tables are readable by a same-user process like the unlocked key.
    Before the first unlock after launch, prod values are not scanned; the first unlock rescans
    the transcripts written in between.
- **Storage sits behind a `ValueStore` interface** (read, write, list versions) from the first
  commit, so 1Password can be a second backend without a rewrite (§16 decision 3).
- **Dev and prod builds get separate entries**, the same split as `accounts/vault.rs` rule 3.
- **Windows: entries are created with local persistence.** keyring's Windows store defaults to
  `CRED_PERSIST_ENTERPRISE`, which **roams** with a roaming domain profile. A data key must
  not leave the machine.
- **The vault file**: `secrets.vault` in the data dir. A versioned header, then one record per
  value, sealed with **XChaCha20-Poly1305**. Each record's associated data is
  `project|env|key|version`, so a record can't be cut out and moved under another name. The
  file is written atomically (temp file, then rename) and is mode 0600 on Unix.
- **Headless Linux, WSL, a box with no Secret Service**: the same rule as `accounts/vault.rs`,
  **no plaintext fallback**. Both data keys can be passphrase-unlocked for the session instead.
  Without that the vault is unavailable and says so.
- **In memory**: delivery paths decrypt a value only for that delivery and hold it in a
  `Secret` type with a hand-written redacted `Debug`, `zeroize`d on drop. The existing `Secret`
  grows into this. **The scrubber and the leak scanner are the exception** (§10.1, §10.2): they
  keep the values they look for in their own tables, and that memory is **not** zeroized. Dev and
  staging tables live while the app runs; prod tables live from the first prod unlock until quit
  (see "What the lock does not cover" above).
- **Crates**: `chacha20poly1305`, `argon2`, `zeroize` (RustCrypto, pure Rust, all three
  platforms). No OpenSSL.

**Backups.** A backup of `secrets.vault` alone is useless elsewhere. A backup that also carries
the keychain (a full macOS backup includes `login.keychain-db`) is protected only by the login
password for dev and staging values.

**Prod values never travel with a backup, by design**: the macOS item is ThisDeviceOnly, and
the Windows Hello key is held in the TPM. A new Mac, a reinstalled Windows, or a Hello or PIN
reset loses every prod value **unless an Export vault was made**, and Export needs the prod
unlock. The panel says this when prod is turned on and offers the export then. (A printable
recovery key is proposed in §16.1.) Moving the vault
on purpose is an explicit **Export vault** with a passphrase, in age's format so the file opens
with the `age` CLI too. **Import vault** is the fifth way a value enters (§1).

## 6. The agent's side: MCP tools

Every tool here is on `OWN_TAB_ONLY_TOOLS` and `PEER_ADDRESSING_TOOLS`
(`claude_code/server.rs`): it acts for the calling tab on a stated identity, and the cards
name that tab as the agent asking. Tools are listed only while the vault is enabled, like the
browser-link tools.

| Tool | Acts at once? | What it does |
|---|---|---|
| `listSecrets` | yes: read-only | The project's keys × environments: set / missing / pending / expiring / overdue, age, provider, targets, the latest delivery per target. **Never a value, a length, or a fingerprint.** |
| `declareSecrets` | yes, for new keys | Adds key definitions (§4.2): names, descriptions, format, `required_in`, `generate`. Names are harmless, and nothing is sent anywhere. Changing a definition the human set, a locked `format`, or a `template` on a key in use needs a card (§4.2). |
| `declareTargets` | **never: a request** | Proposes destinations (§4.4). Widens where values may go, so the human approves. maiTerm checks each address with its adapter before the card is filed (the repo exists, the Railway service exists). |
| `requestSecretValues` | **never: a request** | Opens the entry sheet (§6.1) for named keys × environments. Settles when the human clicks Done. |
| `generateSecret` | dev and staging: yes, if the value is missing. Prod: a request | Rust mints a value from the key's `GenerateSpec`. Replacing an existing value is a rotation (§9), never a generate. |
| `deploySecrets` | per tier policy (§8) | Pushes keys from one environment to one approved target. The reply lists which keys and versions went. |
| `runWithSecrets` | per tier policy (§8) | **maiTerm** runs a command with keys in its environment and returns the masked output (§7.1). There is no mode that hands the agent a ticket. |
| `writeEnvFile` | per tier policy (§8) | Renders a `.env` from the vault to an approved `env_file` target (§7.4). |
| `adoptSecrets` | **never: a request** | Rust reads an existing `.env` (or a JSON/YAML file plus key paths), stores the values, and on approval rewrites the file's lines as references. The agent organizes the cleanup and never reads a value. |
| `captureSecret` | **a request the first time**; a saved recipe reruns inside an approved rotation run | Runs a mint command and stores its output as a pending value, without returning it to the agent. The reply is only "captured" or "failed" (§9.4). |
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
   - **checks as the human types, run in the webview** from rules Rust sends with the sheet
     (none of them secret): the format matches; **a test-mode key in a prod-tier environment,
     or a live key in dev, is flagged** (`sk_test_` in production); leading and trailing
     whitespace is trimmed (the `Secret::new` lesson);
   - **Generate** when the key has a `GenerateSpec`, and **Skip**.
4. **Done** calls one Rust command with the sheet's values. That command **stores them, then
   approves the request, then settles it**, in that order, before returning. Rust then runs the
   checks that need other values (**the same value in two tiers**) and can return warnings the
   human confirms or fixes. The values are never kept in a Svelte store, and the inputs are
   cleared on submit and on unmount. **Cancel** denies the request.
5. The agent is told through the existing settle path (`agent-request-settled` →
   `overlordStore.tellTab`, which never types over a draft), framed as
   `⟦SECRETS⟧ Values set: STRIPE_SECRET_KEY (development, production). Skipped:
   RESEND_API_KEY (development).` From there it carries on: `deploySecrets`, `runWithSecrets`.

**"Enter values" is not `decide`.** In `agent_requests.rs`, approving a request starts a 5-minute
clock (`APPROVED_TTL_MS`) for the window to report back. A human who spends longer typing
would see the request fail. That is why approve and settle happen together, at Done.

**Request lifetime: what has to change in `agent_requests.rs`.** A `secret_values` request should
wait 24 hours, since finding the right dashboard or waiting on a colleague takes time. Today:
- `sweep` expires every request at `requested_at + PENDING_TTL_MS` and ignores `expires_at`;
- the agent's notice hard-codes "lapsed after 30 minutes" (`agentRequests.svelte.ts`);
- docs/mailink-protocol.md §4.4 promises phones 30 minutes.

All three move to a per-kind TTL read from `expires_at`. Because a request that lives a day will
meet a reload (a human's Cmd+Shift+R mints a new tab id), `reloadTab` must **remap the request's
tab id**, like its other `remapTab` calls. A request held for a day must not block the agent's
own reload for a day either. Requests stay in memory: after a restart the agent's
`getRequestStatus` says the request is gone, and the agent asks again.

**The webview holds a value for an instant.** It is the human's own machine, and the
alternative, a native dialog per platform, costs three implementations. The rules: never in a
Svelte store, never logged (dev builds attach the console), never in an error message.
**Dev builds load `tauri-plugin-mcp-bridge`**, whose `webview_execute_js` and IPC-capture tools
let an agent read the sheet's inputs and payloads. So the sheet **refuses prod-tier values in a
build with the bridge**, and warns for dev and staging ones.

## 7. Delivery: getting a value where it needs to be

Every delivery path follows the same four rules:
- **stdin or an API, never argv.** A test runs every adapter against a fake binary and fails if
  any value shows up in argv.
- **Never typed into a PTY.**
- **Nothing that returns to an agent is unscrubbed**: output, errors and settle outcomes all go
  through the value scrubber (§10.1).
- **Recorded in the ledger.**

### 7.1 Running a process with secrets

The equivalent of `doppler run`. Two forms:

**Agent runs (`runWithSecrets`).** maiTerm runs the command itself, in its own process group as
watch scripts are (`watch.rs`), with the values in the child's environment, and returns the
**masked** output as the tool result. Masking (1Password's idea) replaces any value the run
holds, plus its base64 (all three alignments), URL-encoded, hex and JSON-escaped forms, with
`«STRIPE_SECRET_KEY»`. **This is the only form an MCP tool offers.** No tool hands an agent a
ticket or anything else it could redeem for values: the first draft's ticket mode would have
let an agent speak the shim's protocol itself and skip the masking.

**Service runs (the `maiterm-run` shim).** A stack service with secrets (§7.2) is started by a
small shim that maiTerm types into the service's shell:

```
'/Applications/maiTerm.app/Contents/MacOS/maiterm-run' --ticket <t> -- sh -c '<service command>'
```

- **A separate small console-subsystem executable**, shipped beside maiTerm and typed by
  **absolute path**. It is not the main binary: in release builds `main.rs` sets
  `windows_subsystem = "windows"`, and a GUI-subsystem exe started from cmd or PowerShell has no
  console, so the shell neither waits for it nor passes Ctrl-C. The argv-dispatch precedent
  (`run_browser_link_host_if_requested`) works only because Chrome supplies pipes.
- **The ticket** is single-use, valid for 60 seconds, and bound to the exact command string, the
  folder and the key set. maiTerm types it, so it sits briefly in the service's scrollback and in
  `getServiceOutput`.
- **A ticket is worth nothing to anyone but the shim.** On redemption maiTerm identifies the
  peer **from the OS, not from the message**: `LOCAL_PEERPID` (macOS), `SO_PEERCRED` (Linux),
  `GetNamedPipeClientProcessId` (Windows). The peer's executable must be maiTerm's own
  `maiterm-run`, and its argv and cwd are read from the OS. The values are released only to that
  pid.
- **On Unix** the shim `exec`s `sh -c`, so the process tree is exactly what it would be without
  it. **On Windows** there is no `exec`: the shim spawns the child (through `cmd /c`), forwards
  Ctrl-C/Ctrl-Break and returns the child's exit code.
- **On a TTY** (an interactive dev server) the shim can't mask without breaking the terminal,
  so masking is off there. That output still reaches agents only through the scrubbed MCP
  boundary (§10.1).

**What an approval does NOT cover, said plainly on the card:** it binds the command line, not
the code it runs. An agent that can edit `scripts/seed.js` can make the approved
`node scripts/seed.js` print `process.env`, or write it to a file. Also, a running process's
environment can be read by the same OS user (`ps eww`, `/proc/<pid>/environ`). Local execution
with a secret means trusting the code with it, which is why prod-tier keys don't run locally
by default (§8).

### 7.2 Stack services

Today a service starts by typing `cd`, `export`s and the command into its shell
(docs/stack.md §4). **A secret cannot be delivered that way**: it would land in the
scrollback, the shell history and the scrollback database. So:

- `Service` gains `secrets: Vec<{env_var, key}>` and `secret_env: Option<String>` (which vault
  environment). This is the service's only binding: there is no `stack_service` target, and
  nothing to `deploySecrets` to. Delivering a new value means restarting the service.
- When a service has secrets, its start types the shim line of §7.1, with a fresh ticket each
  start and restart. **The command goes in as `sh -c '<command>'`**, quoted, because
  `Service.command` is typed verbatim: `npm run build && npm start`, a pipe or a `cd x;` would
  otherwise be split by the shell, and everything after the first `&&` would run without
  secrets. The plain `env` exports stay as they are for non-secret values.
- A rotation restarts every service in its ledger (§9.2).
- The shell underneath never holds the values. Only the service process does, so a Ctrl-C in
  the console leaves nothing behind.

### 7.3 Deploy targets (adapters)

Each `kind` is an adapter in `src-tauri/src/secrets/targets/` with five operations: `check`
(the address exists and maiTerm can reach it), `push`, `list` (which names are there and when
they changed; providers never return values), `remove`, and `verify`.

**An agent must not be able to change where an approved push goes.** Each of these CLIs picks
its target from files and PATH an agent can edit: `gh` from the cwd's git remote, `wrangler`
from `wrangler.toml`, `vercel` from `.vercel/project.json`, `fly` from `fly.toml`, `kubectl` from
the current context, and the binary itself from PATH (a fake `gh` in `~/.local/bin` would get
the value on stdin). So every adapter:
- passes **every address field as an explicit flag** from the approved `SecretTarget.address`;
- runs in an **empty temporary directory** with a **minimal environment**;
- runs the CLI by the **absolute path resolved when the target was approved**, shown on the card.

| Target | How the value travels | Address pinned by |
|---|---|---|
| GitHub Actions (repo, environment, org) | `gh secret set NAME` reading **stdin** | `-R owner/repo`, `--env`, `--org` |
| Cloudflare Workers | `wrangler secret put NAME` reading **stdin** | `--name`, `CLOUDFLARE_ACCOUNT_ID` |
| Vercel | `vercel env add NAME <environment>` reading **stdin** | `--scope`, project id |
| Fly.io | `fly secrets import` reading `NAME=value` lines on **stdin**, with `--stage` (otherwise it redeploys at once, which a rotation plans separately) | `-a app` |
| Railway | the GraphQL API (`variableUpsert`). The CLI's `--set K=V` puts the value in **argv**, so it is not used | project, environment, service ids |
| AWS Secrets Manager / SSM | the API, or the CLI with `--secret-string file:///dev/stdin` (Unix) | ARN, region, profile |
| Kubernetes | `kubectl create secret … --dry-run=client -o yaml \| kubectl apply --server-side -f -`. A client-side `apply` keeps a **second copy** of the data in the `last-applied-configuration` annotation | `--context`, `--namespace` |
| `env_file`, `ssh_env` | local, see §7.4, §7.5 | — |

Each row must be checked against the real CLI or API before its adapter ships. **Order** (§16
decision 4): GitHub Actions, Cloudflare, Railway; then Vercel and Fly; AWS and Kubernetes later.

**Adapter errors are fixed sentences.** An adapter reports "GitHub refused the push: not
authorized for maisoft/api", never raw stderr. Provider errors can echo their input, and
outcome text travels a long way: `carryOut` puts the error message into the request's outcome,
which `onSettled` types into the agent's tab, shows in a toast and sends to the phone.

**A platform's own credential (a Railway token, a Cloudflare API token) is itself a vault key**,
marked `deployer`. Deployer keys are never delivered anywhere except to their own adapter.
Where the human is already signed in to the CLI (`gh auth`), the adapter uses that and stores
nothing.

**OIDC advisor.** When a GitHub Actions target holds a cloud key (`AKIA…`, a GCP
service-account JSON), the panel suggests federating with OIDC and dropping the key. It is a
suggestion, nothing is changed.

### 7.4 `.env` files

The weakest destination, and the one every framework reads. Rules:
- policy by tier (§8). A prod-tier file needs the human to enable it per target, with a card
  that says "a production value will be on disk in plain text";
- the path is **pinned when the target is approved**, after canonicalizing its parent directory,
  so a symlinked parent can't move the write outside the project;
- the file must be **ignored by git** (`git check-ignore`), checked every write. An agent can edit
  `.gitignore`, so this guards against accidents, not against the agent;
- mode 0600, written atomically;
- the file gets a header naming maiTerm as the writer, and the ledger knows the file, so a
  rotation rewrites it.

The panel nudges toward `runWithSecrets` and service secrets, which leave nothing on disk.

### 7.5 SSH hosts

For an ssh tab, the place is `user@host:folder`, the same identity watch scripts use
(`watch::place`). Values go **over the stdin of maiTerm's own ssh connection**, through the
tunnel's ControlMaster socket. `runWithSecrets` on an ssh tab feeds `sh -s` on the host a
script that holds the values **in shell variables only**, exports them to the command and
returns its output over the same stream.

**This is stricter than the patterns it borrows from**, which both write to the remote disk:
- `watch/remote.rs` writes the script and its `.out`/`.err` files under
  `~/.maiterm/watch-scripts`;
- the setup-token handoff writes a file under `~/.maiterm/tokens` that is sourced, then removed
  (`accounts/remote.rs`).

Here nothing is written to the remote disk (no heredoc'd file, no temp output) unless the target
is an `env_file` there. The card says **which host**, and that root and the same user on that
host can read the process's environment.

### 7.6 Templates and references

A key may be a template over other keys (`DATABASE_URL =
postgres://{{DB_USER}}:{{DB_PASS}}@db:5432/app`). The agent may write the template on a new key,
since it holds no secrets, and Rust fills it in at delivery. Once the key has a value or a
target, a template change needs a card (§4.2), and a target's allowlist is checked against
every key a template uses (§4.4). Rotating `DB_PASS` re-renders `DATABASE_URL` everywhere it
went.

## 8. Environments and policy

Policy goes by tier. These are the defaults, and the human can change them per project in the
panel. **No MCP tool can change policy**, and policy is not in `preference_meta`.

| | dev | staging | prod |
|---|---|---|---|
| `deploySecrets` to an approved target | at once | at once | **a card each time**, listing keys and version changes (`STRIPE_SECRET_KEY v3 → v4`) |
| `runWithSecrets` | a card once per command + place, then at once | same as dev | **off**. The human can turn it on, then it is a card per run |
| `writeEnvFile` to an approved file | at once | a card each write | **off**. Enabled per target with a card, then a card each write |
| `generateSecret` (missing value) | at once | at once | a card |
| `captureSecret` | a card the first time per command + place + `extract` | same | a card each time |
| reveal / copy in the panel | yes | yes | after the prod unlock (§5: Touch ID / Windows Hello / passphrase) |
| leak found | reported to the human | reported, rotation proposed | reported, **rotation proposed as a Decision** |

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
   Stripe dashboard"). The previous value is purged **7 days after the revoke** (counted from
   the revoke, not the promote: a long overlap would otherwise purge a value a revoke recipe
   still needs).

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
  files under the project's paths and the human's other projects, and reports to the **human**
  "found in ~/old-project/.env" without showing what it found. The agent sees only that a
  consumer was added to the plan.
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

Many providers have a CLI or API that mints keys, and the value comes back on stdout. That is
exactly where an agent must not see it. `captureSecret({key, env, command, extract})` runs the
command through the agent-run path of §7.1 and **stores** the output instead of returning it.

What keeps it from being an oracle (§1 rule 4):
- **`extract` (a JSON path or regex) is part of what is approved**, with the command and the
  place. Otherwise an agent reruns an approved capture with `^sk_live_a`, `^sk_live_b`… and
  reads the output one bit per call.
- **The reply is "captured" or "failed"**, with no length and no "matches the format".
- **Captures are rate-limited** per key.

A revoke command works the same way, with the old value going *in* through stdin or a template.
Recipes that work can be saved on the key's `RotationPolicy`, so the next rotation of that key
needs only the start card. (Stripe, for one, has no public API that mints or rolls a secret key
as far as we know, so its keys are human-minted, §9.5.)

### 9.5 Provider packs

A small built-in table per provider: format prefixes (test vs live), the console URL for
issuing and revoking keys, whether two keys can be live at once, typical expiry, and which
adapter or recipe mints and revokes, or "human". First: Stripe (human-minted), GitHub,
Anthropic, OpenAI, Cloudflare, AWS (§16 decision 4). Later candidates: Slack, SendGrid, Resend,
Supabase. **Every entry
must be checked against the provider's current docs when it is written**, not from memory.

## 10. Keeping secrets out of sessions

### 10.1 What maiTerm can and can't stop

- **Every boundary maiTerm owns is scrubbed** for live values (all the forms in §7.1):
  - **what it returns**: `getTabContext`, `getServiceOutput`, the Overlord's tab reads,
    maiLink's `recent_text()` fallback, tool results, and request outcomes;
  - **what it persists**: the scrollback database write, its logs, toasts, the phone mirror and
    share files.

  A TTY service with masking off commonly prints its config (a `DATABASE_URL` at boot), and that
  reaches agents through exactly these tools.
- **Where someone else owns the store, maiTerm reports.** Claude Code's transcript is Claude
  Code's file, and **anything in it has already been sent to the model provider**. Editing the
  file does not undo that. So a value found there is treated as leaked, and the response is
  rotation, not cleanup.
- **A same-user agent's own tools are not stopped** (§1): `cat .env`, the keychain CLIs and
  `ps eww` all work for it. §10.3 makes them rare and visible. It doesn't make them impossible.

### 10.2 Leak detection

**A dedicated scanner**, not maiLink's tail: `mailink/transcript.rs` skips tool outputs (exactly
where `cat .env` or `ps eww` output appears), reads only a tail, and runs only when maiLink asks.
The scanner holds the values in memory (not zeroized; prod ones only from the first prod
unlock after launch, §5) and runs
one Aho-Corasick pass, in raw, JSON-escaped, URL-encoded, hex and three-alignment base64 forms,
over:
- Claude JSONL and Codex rollouts, read **whole and incrementally, `tool_result` included**,
  whether or not maiLink is on. Gemini has no transcript source yet;
- the terminal grid at the scrollback-database write;
- the git diff of a commit, if the human enables the optional pre-commit hook;
- files under project paths, when asked during discovery (§9.2).

A hit reports **which key, which environment, where and when, to the human**, never the
surrounding text. Per §8 it raises a rotation proposal. **The agent is not told about hits on
text it produced**: otherwise an agent could print guesses and use the scanner (or the masking)
to confirm them, which matters most for the low-entropy passwords §4.3 worries about. Values
under 12 characters are skipped, since they would match by accident. The panel notes this.

### 10.3 Making the wrong thing hard

- `adoptSecrets` turns an existing `.env` into references, so there is less on disk to read.
- **A `PreToolUse` deny.** maiTerm already receives `PreToolUse` as an http hook (`hooks_handler`
  in `claude_code/server.rs`). It has **never decided anything** with it (it answers an empty
  200: `claude_code/lockfile.rs`). This would be its first deny. The deny list:
  - Bash or Read on a file the ledger says holds values;
  - the keychain CLIs (`security find-generic-password`, `secret-tool`, PowerShell credential
    modules / `CredRead`);
  - process environment reads (`ps e…`, `/proc/*/environ`).

  The reason it gives points the agent at `runWithSecrets`. This is a **speed bump**: it fails
  open (when maiTerm is down or the tunnel is dead, the call goes ahead), and string matching is
  easy to dodge (`cat .e""nv`). To verify: that a deciding `PreToolUse` works on every runtime
  we support (Codex hook parity is listed in docs/codex-integration-review.md).
- The ledger-file part of this deny ships **with `writeEnvFile` and `adoptSecrets` (phase 4)**,
  not with leak detection: from the moment maiTerm writes a file holding values, something must
  stand in front of it.
- **Claude's own `Read` deny rules** for the ledger's files and the vault file: **reopened, see
  §16 decision 6.** They are stronger than the first draft said. Per Claude Code's permissions
  docs they apply to the built-in file tools, to the file commands Claude Code recognizes in
  Bash (`cat`, `head`, `tail`, `sed`, `tee`) and to redirection targets. What they miss is a
  command that reads without naming the file (`grep -r`, a script). Unlike the hook, they hold
  when maiTerm is down.
- **For prod, the boundary that holds** is the agent's own sandbox or a separate OS user (§1).
  Claude Code's sandbox **does nothing for this out of the box**: its default read policy is
  "most of the machine, including credential files", sandboxed commands inherit the
  environment, it does not cover the Read tool, hooks or MCP servers, and **it does not run on
  native Windows** (WSL2 only). So the one-click "Run this project's agents sandboxed" offer
  must write a specific config, in the project's local settings:
  - `sandbox.credentials` entries (or `sandbox.filesystem.denyRead`) for the vault file and every
    ledger file;
  - `Read` deny rules for the same paths, since the sandbox doesn't cover the Read tool;
  - `sandbox.credentials` `envVars` entries for the env var names the vault delivers, where an
    agent's own shell might hold them.

  The spike (§16 decision 7) proves that config blocks the vault file, the ledger files, the
  keychain and other processes' environments. Its review needs a scratch HOME. Native Windows
  gets no sandbox offer: the panel says to run agents in WSL2 for prod.
- The priming line (`initSession`) tells agents the vault exists and which tools to use, so
  "paste your API key here" is not the agent's first idea.

## 11. UI

### 11.1 The Secrets panel

A full-window view per project, like Preferences, opened from the workspace's context menu, a
sidebar row and every secrets card. **No shortcut in v1, and not a new `tab_type`** (that would
add a value an older build can't load; §16 decision 8). It needs the width, so it is not a side
dock:
- **a matrix of keys × environments**, each cell showing set (with age), missing (red when
  required), pending (a rotation in progress), overdue, or expiring;
- expanding a key shows its description (quoted as the agent's), provider, template, targets,
  version history (no values), consumers from the ledger, and its audit rows;
- actions: Set, Generate, Rotate, Remove from target, Reveal, Copy.

**Reveal** shows the value for 10 seconds. **Copy** clears the clipboard after 30 seconds if it
still holds that value, and keeps the value out of clipboard history:
`CanIncludeInClipboardHistory=0` and `CanUploadToCloudClipboard=0` on Windows,
`org.nspasteboard.ConcealedType` on macOS. Both are written to the audit log. Values are masked
by default, because screens get shared.

### 11.2 Cards

New agent-request kinds, each rendered by `AgentRequestCard` with the existing guards (no
decision in a card's first 1.5 s, the second click of a double-click ignored):
`secret_values`, `secret_targets`, `secret_deploy` (prod), `secret_run`, `secret_env_file`,
`secret_adopt`, `secret_capture`, `secret_definition` (a locked field or template change, §4.2),
`secret_rotation`, plus the rotation steps that need the human. As with every card: **the words
are maiTerm's, the agent's only as a quoted `reason`; they must be true about what runs and
where; and they never carry a value**, since cards reach the phone.

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

- **Approvals: yes.** `secret_targets`, `secret_deploy`, `secret_run`, `secret_env_file`,
  `secret_adopt`, `secret_capture`, `secret_definition`, `secret_rotation` and revoke
  confirmations are agent requests, so they appear in maiLink §4.4's `/requests` like any other
  kind. A new kind alone is **not** a protocol bump: the phone draws any kind from its text
  fields (docs/mailink-protocol.md §4.4, `agent_requests.rs`).
- **Values: no, in v1.** Entering a value on the phone would carry it over the maiLink LAN API
  and through the phone app. But the phone shows Approve/Deny on every pending request, and a
  phone Approve on `secret_values` would `decide` with no values entered. So:
  - `decide(by: "phone")` **refuses** `secret_values` and any human rotation step that needs a
    value pasted;
  - the request carries a wire flag (`desktopOnly: true`) so the phone draws "Enter these on
    the desktop" instead of buttons. **That field is a protocol bump.**

  The "never go to desktop" rule (memory) is broken here on purpose, and the card says why.
- **Values on the phone later, end-to-end encrypted** (§16 decision 1), after the targets phase.
  The real use is rotation: you create the new key in a provider's mobile dashboard or your
  phone's password manager and paste it. The desktop card carries a one-time X25519 public
  key; the phone seals the value to it, and only Rust can open it. The design:
  - **The public key is trusted because the card arrives over maiLink's pinned TLS** (protocol
    §3.1). The sealing protects the value; the pin protects the key from being swapped. The
    relay never carries a card, so it never sees the key.
  - **The ciphertext is bound** to the request id, key name, environment and expiry as
    associated data, so a value sealed for one key or request can't be submitted for another.
  - **The private key is single-use and request-scoped**, destroyed on settle or expiry.
  - **The phone must not copy the value**: on iOS, Universal Clipboard would sync it to the
    Mac's clipboard, where a same-user agent can read it with `pbpaste`.

  The request keeps `desktopOnly` until then.
- **A prod approval from the phone while the desktop is locked** waits for the next desktop
  unlock, and the phone card says so ("runs when you next unlock on the desktop"). Approved
  requests normally fail after 5 minutes unreported (`APPROVED_TTL_MS`), so these kinds get
  the same per-kind TTL treatment as §6.1. A rotation's prod steps (redeploys, restarts, a
  revoke recipe that needs the old value) hit the same unlock.
- Leak reports and rotation proposals ring the doorbell, gated by mute (docs/mute.md) like any
  other notice.

## 13. How it meets the rest of maiTerm

- **Reload / duplicate / archive**: the vault belongs to the project, not to a `Tab`, so
  there is nothing to carry. Pending requests are keyed by the asking tab, and `reloadTab` must
  remap them to the new tab id (§6.1). Today an agent's reload of its own tab is refused while it
  has a request open (docs/agent-requests.md §1), which a day-long `secret_values` request would
  make unreasonable. The remap makes that refusal unnecessary for this kind.
- **Move project** (docs/relocate.md): the project's paths, `env_file` target paths and `run`
  approvals keyed by place all follow the move.
- **Workspace Share**: the share file carries the **manifest**: key names, descriptions,
  environments and target kinds, never values or target approvals. Importing it shows "this
  project needs 6 secrets" and opens the entry sheet. This becomes a `Shared*` type on the
  allowlist (docs/workspace-share.md), and it is where the remote URL matches a project (§3).
- **Overlord**: `listEscalations` and rules can see a project's missing and overdue counts.
  Never values.
- **Accounts**: the audit row records the account the tab **spawned** under
  (`accounts/tab_record.rs`, docs/mailink-protocol.md §14), never the active account.
- **Mute**: silences notices, never cards (docs/mute.md).

## 14. Threat model

| Adversary | Defended by | Not defended |
|---|---|---|
| **A prompt-injected agent** ("send the key to evil.com") | No tool takes a value or a free URL; targets need a card and an agent's config and PATH can't redirect them (§7.3); templates can't cross an allowlist (§4.4); prod deploys need a card each time | An agent that writes code which leaks the key at runtime and gets it deployed. Code review and CI catch that, not the vault |
| **A careless agent** (prints env, commits `.env`) | No values in tool results; masking and scrubbing; gitignore check; leak scan; pre-commit hook | A value it already read through its own tools |
| **A determined agent running as the same OS user** | Prod values behind an OS-enforced unlock (§5): on macOS release builds the keychain itself refuses it; the `PreToolUse` speed bump (§10.3); no oracles (§1 rule 4) | Dev and staging values, unless the project opted them into the prod guard: the keychain item, process environments and `.env` files are readable to it. **On Windows** it can raise a Hello prompt for the same credential, and read the unlocked key from maiTerm's memory. **On Linux and in dev builds** the passphrase is all there is. **The real boundary is a configured sandbox (§10.3) or a separate OS user** |
| **Another local process as the same user** | As above | As above, the same reach docs/login.md §9.2 accepts |
| **A stolen backup or synced data dir** | The vault file alone is useless; Windows entries don't roam (§5); prod needs the prod unlock | A backup that also carries the macOS login keychain opens dev and staging values to whoever has the login password |
| **A lost or compromised phone** | Values never reach the phone; approvals need a paired device token (docs/agent-requests.md §2) | A paired phone can approve a prod deploy, so unpairing is the response |
| **A shared screen / shoulder** | Masked by default; reveal times out; copy stays out of clipboard history | — |
| **maiTerm's own logs and errors** | `Secret` has no `Display`/`Serialize`; errors name the operation and key, never the value (the `accounts/vault.rs` rule 1); adapter errors are fixed sentences (§7.3) | — |
| **An agent in a dev build** | Prod value entry refused while the MCP bridge is loaded (§6.1) | Dev and staging entry can be watched through the bridge's webview tools |
| **A malicious dependency in the app being run** | — | It gets whatever the process gets. That is true of every secrets manager |

**The vault keeps secrets out of sessions and decides where they go. It does not make the code
that uses them trustworthy, and it does not contain an agent that shares the human's OS
user.** Every card on a `run` or prod path says so in one line.

## 15. Rejected

- **One keychain item per secret**: §5.
- **Giving the agent the value and asking it not to print it**: it ends up in the transcript,
  so with the model provider, the moment the agent reads it.
- **Typing `export`s into a PTY**: scrollback, history and the scrollback database (§7.2).
- **Handing agents a run ticket** (the first draft's `maiterm run --ticket` for agents): a ticket
  is redeemable by whoever holds it, the binding was self-reported, and the agent could skip the
  masking (§7.1).
- **Trusting a CLI's own config for the destination**: the agent can edit it (§7.3).
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

## 16. Decisions (Darryl, 2026-10-23)

The first draft's open questions, each with the answer agreed and the reason it was chosen.
**"v1" means the first release: phases 1–5 of §17.** A review of this section (of 8e872d5c)
corrected the facts under decisions 2, 6 and 7. Decision 6's reason turned out to be wrong, so
it is reopened in §16.1.

1. **Phone value entry: not in v1; later, end-to-end encrypted** (§12). The need is real only
   once rotation exists (pasting a newly minted key from a mobile dashboard or a password
   manager). Sealing the value to a one-time key on the desktop card protects the value; the
   card's public key is protected by maiLink's pinned TLS, which the relay never carries.
2. **Prod unlock: OS-enforced, not a dialog** (§5). macOS release builds: a data-protection
   keychain item, ThisDeviceOnly, requiring user presence (needs a provisioning profile).
   Windows: a key derived from a Hello signature. Linux and dev builds: a passphrase. Unlocked
   until 15 minutes without human-initiated use. Dev and staging can opt in per project, not by
   default. A passphrase on every prod action gets turned off within a week, and only the
   OS-enforced form also stops a same-user agent, fully on macOS and partly on Windows (§5
   lists the gaps).
3. **1Password: designed for from phase 1, built right after targets.** Storage sits behind a
   `ValueStore` interface from the start (§5); the 1Password backend resolves with `op read` and
   writes back on rotation, while manifest, targets, ledger and runs stay in maiTerm. Doppler and
   Infisical wait for someone to ask. 1Password is what most developers already have and is
   teams' sharing for free; retrofitting a second backend later is a rewrite.
4. **Targets: GitHub Actions, Cloudflare, Railway; then Vercel, Fly; AWS and Kubernetes
   later** (§7.3). **Provider packs: Stripe, GitHub, Anthropic, OpenAI, Cloudflare, AWS** (§9.5).
   GitHub is near-universal, maiTerm itself deploys to Cloudflare, Railway is in use here, and
   AWS/Kubernetes are the hardest adapters and most likely to have their own secrets manager.
5. **Prod `run`: off by default; enabled per project, then a card per run** (§8). Prod
   credentials on a laptop are the anti-pattern this ends; prod migrations belong in the deploy
   pipeline; the rare one-off ops job fits a card per run.
6. **Reopened, see §16.1.** It was "no `Read(.env)` rules in Claude's settings", because such a
   rule "covers only the Read tool". That is false (§10.3).
7. **The sandbox is offered as one click, after a spike proves it** (§10.3). Out of the box the
   sandbox protects none of this, so the offer writes a specific config (credentials or
   `denyRead` entries plus `Read` deny rules for the vault file and ledger files). The spike proves
   that config blocks the vault file, the ledger files, the keychain and other processes'
   environments. It ships only if the spike passes, since otherwise the button would promise
   protection it doesn't give. Native Windows gets no offer: the sandbox runs only on macOS,
   Linux and WSL2.
8. **The panel is a full-window view like Preferences, with no shortcut and no new `tab_type`**
   (§11.1). The keys × environments grid needs width, it is an occasional screen mostly reached
   from a card, and a new `tab_type` value is the older-build-loads-empty risk for no gain.
9. **Dynamic secrets: advice, not a feature.** When a long-lived `AKIA…` key is stored, the panel
   suggests AWS SSO (`aws sso login` already yields short-lived credentials) or OIDC for CI, as
   the OIDC advisor does (§7.3). The providers that support short-lived credentials ship the
   tooling; maiTerm's job is to stop the long-lived key being stored, not to reimplement STS.
   (The SSO token cached in `~/.aws/sso/cache` is itself a bearer credential an agent can read;
   the advice says so.)

### 16.1 Waiting on Darryl

1. **Decision 6, reopened: write `Read` deny rules for the vault file and the ledger's files?**
   Recommended: **yes, into the project's local, uncommitted Claude settings**, written by
   maiTerm when it first writes a file holding values. Claude enforces these rules itself, even
   when maiTerm is down (the hook fails open), and they cover the built-in tools, `cat`/`head`/
   `tail`/`sed`/`tee` and redirections. They miss indirect reads (`grep -r`, a script), which is
   what the sandbox is for. The cost is that maiTerm writes Claude settings, so the change is
   reviewed with a scratch HOME. These are the same rules the sandbox offer writes (decision 7),
   so this applies them without the sandbox too.
2. **A printable recovery key?** Prod values are lost with the device unless exported (§5). A
   recovery key, made once at setup and printed or kept offline, would unwrap the data keys.
   The trade-off: it is a second way in, so a same-user agent that finds it has everything,
   which is the attack §5's migration closes for the passphrase. Recommended: offer it, never
   store it on the machine, and say so where it is made.

## 17. Build order

1. **Vault core.** `src-tauri/src/secrets/`: the envelope store behind the `ValueStore`
   interface, with the two data keys and local-only Windows persistence (§5), the passphrase
   unlock (the Linux path and the fallback everywhere), the data model, the Secrets panel as a
   full-window view with human entry, environments, the audit log. `accounts/vault.rs`'s `Secret`
   grows `zeroize` and moves to a shared module.
2. **The agent manifest.** `listSecrets`, `declareSecrets` with its locked fields,
   `requestSecretValues`, the entry sheet, the combined store-approve-settle Done, per-kind
   request TTLs and the reload remap (§6.1), the phone refusal and `desktopOnly` (§12), the
   ⟦SECRETS⟧ notice, `generateSecret`. **This is the first phase an agent can use, and it proves
   Darryl's flow end to end.**
3. **Prod guards, before anything can deliver a prod value.** The OS-enforced prod unlock (§5):
   on macOS the data-protection keychain item, **with the release pipeline's entitlement and
   Developer ID provisioning profile**; on Windows the Hello-derived key. The re-wrap that
   deletes phase 1's passphrase wrap. The detection-vs-use split for prod match tables. The
   sandbox spike (§10.3), and the one-click offer only if it passes.
4. **Local delivery.** The scrubber at every MCP and persistence boundary (§10.1) **first**,
   then `runWithSecrets` with masking, the `maiterm-run` shim with peer-verified tickets for
   stack services (§7.1, §7.2), `writeEnvFile`, `adoptSecrets`, and **the ledger-file
   `PreToolUse` deny with them** (§10.3).
5. **Targets.** `declareTargets`, `deploySecrets`, the ledger, adapters with pinned addresses and
   binaries (§7.3): GitHub Actions, Cloudflare, Railway. The OIDC / AWS SSO advice, which is
   about targets. **The end of v1.**
6. **1Password backend** on the `ValueStore` interface (§16 decision 3).
7. **Rotation.** Runs with human mint and revoke, `requestRotation`, the run view, age and
   expiry proposals, the first provider packs (§9.5), end-to-end encrypted value entry on the
   phone (§12).
8. **Leak detection.** The dedicated transcript scanner, scrollback and pre-commit scans,
   leak-triggered rotation, the rest of the `PreToolUse` deny (keychain CLIs, process
   environments).
9. **Automation and reach.** Capture recipes with their oracle guards, adapter mint/revoke, ssh
   delivery, Vercel and Fly, then AWS and Kubernetes.

Each phase is reviewed before the next. The shim and the adapters must be reviewed with a
scratch HOME. **Two tests must exist before the first adapter ships**: no value in argv, and no
change to an agent-writable file or PATH entry moves where a push goes.
