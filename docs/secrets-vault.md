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

**Reshaped 2026-10-28: a provider first.** maiTerm is the layer between agents and the human's
secrets manager. Values live in **1Password or Doppler** whenever one is connected (§5.1);
maiTerm's own vault is the **fallback** for people without one, with a label saying plainly what
it protects (§5.2). **Every key gets the same protection, whatever its tier**: a "dev" key is
often a live key. **Prod-tier values need a provider** (§16 decisions 11–13). Darryl's reasoning:
"it basically is adding a much better integration layer on to Doppler and 1Password".

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
and knows where to look. A provider doesn't change that on its own: 1Password and Doppler
also hand values to anything running as the human once they are signed in (§5.1). The real
boundaries are a gate the provider enforces per program (1Password's app authorization, which
is how prod values are read, §5.1), running agents sandboxed (Claude Code's sandbox), or a
different OS user. Every card on a prod path says this in one line.

Everything else follows from the rule:

1. **No MCP tool returns a value, and no tool accepts one.** Through maiTerm, values reach the
   store (the provider or the local vault) in five ways only: the human types them into
   maiTerm's own entry sheet, Rust generates them, Rust captures them from an approved command's
   output, Rust reads them out of a file (§6), or the human imports a local-vault export (§5.2).
   The human can also set values in the provider's own app, which maiTerm then reads.
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

**Where 1Password and Doppler stand in 2026** (checked 2026-10-28):
- **1Password** moved into this space. **Environments** (beta) group a project's secrets, and
  can mount a `.env` that is never on disk (Mac and Linux). An **Environments MCP server**
  lets Codex, Cursor and Kiro agents create Environments and list names, "doesn't read or return
  secrets to the AI agent", and requires "a secure 1Password authorization prompt … for every
  action". `op run --environment` injects into one process. **NVIDIA OpenShell** (beta) gives a
  sandboxed agent placeholders, and a gateway swaps in real secrets for approved hosts only.
  Third-party roundups add Unified Access for agent credentials (March), a Credential Broker
  minting per-run GitHub Actions credentials (preview, July) and Developer Watchtower, which
  finds plaintext `.env` files.
- **Doppler** has the strongest environment model (dev/staging/prod configs), change requests
  with approvals and 30+ sync integrations (Vercel, AWS, GitHub Actions, Kubernetes). Reviewers
  rate its agent support "env injection only": it has no idea of an agent's identity.

**What maiTerm adds that neither has is the agent's side of the loop.** The agent does the
legwork (declares what the code needs, deploys, restarts, verifies, runs the rotation
checklist), and the human makes the few irreversible decisions on cards built from maiTerm's
own state. Around that:
- delivery into stack services and ssh hosts;
- a ledger of every delivery, which drives rotation runs;
- leak scanning of the agents' own transcripts;
- read-deny rules across every agent runtime;
- phone approvals, and noting what reached an agent before it asked.

So maiTerm builds that layer, and leaves storage, team sharing, recovery and sync to the
provider (§5).

## 3. Shape

```
Secret project  (bound to local folders the human approved; the repo's remote URL only matches shares)
├── store          where its values live: a provider connection (1Password, Doppler) or the local vault (§5)
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
in the project's store (§5): the provider, or the local vault file.** Kinds and labels are **strings, not serde enum variants**: a new
variant makes an older build load empty state (memory: "New serde enum variant = downgrade
wipe").

### 4.1 Project and environments

```rust
SecretProject {
    id, name,
    paths: Vec<String>,             // canonical local roots the human approved (§3)
    remote: Option<RepoRoot>,       // remote URL + subpath: share-import matching only
    store: StoreBinding,            // "local", or a provider connection id + per-environment
                                    // mapping (a 1Password vault/item, a Doppler project/config) (§5.1)
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
    kind: String,                   // "string" | "file" (a service-account JSON, a .p8, a TLS key: §7.1)
    path_var: Option<String>,       // file kind: the variable that gets the PATH, "GOOGLE_APPLICATION_CREDENTIALS"
    risk: String,                   // "live" (default) | "low": a test-mode prefix, a localhost URL,
                                    // or the human's say-so. Never set by an agent (§5)
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
ValueMeta {                         // plaintext metadata; the value itself is in the store
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

## 5. Where values live: a provider first, the local vault as the fallback

> **Values live in the human's secrets manager (1Password or Doppler; Infisical later)
> whenever one is connected. maiTerm's own vault is the fallback for people without one, labelled
> for what it does and doesn't protect. Prod-tier values need a provider.** (§16 decisions
> 11–13, 2026-10-28.)

**Why a provider first.** What only maiTerm can do is the layer around the store (§2): the
cards, the ledger, delivery, rotation runs, leak scanning and read-deny rules. Storage, team
sharing, recovery, audit and sync to hosting are what 1Password and Doppler already do, and do
better. Guarding a live key against an agent running as the human needs a gate the OS or the
provider enforces. 1Password's app authorization is one. Building our own equally on macOS,
Windows and Linux is the heaviest and weakest part of the earlier design (§5.3).

**Every key gets the same protection, whatever its tier** (decision 12). A "dev" AWS key can
still create real infrastructure; a "staging" database is often a copy of prod; a "dev" LLM key
bills the same account. So every key is **live** by default (`KeyDef.risk`, §4.2), and is marked
**low** only when it provably can't do harm: a test-mode prefix from a provider pack (§9.5:
`sk_test_`), a `localhost` connection string, or the human's own say-so. **The tier decides
where a key may go and what needs a card (§8), never how well it is stored.**

### 5.1 Provider connections

A connection holds **maiTerm's own narrow credential**, never the human's login. Facts below
were checked against each provider's docs on 2026-10-28.

**1Password**
- **Dev and staging: a service account** (`ops_…`), limited to the project's own vaults.
  Service accounts work on every plan, Individual and Families included. Two limits shape the
  layout:
  - they can't reach the built-in Personal/Private vault;
  - they can only **read** Environments.

  So maiTerm uses **dedicated vaults** per project and tier ("maiTerm · api · dev"), with one
  item per environment and one concealed field per key. A service account can read and write
  those. Environments can be added as an extra read source. Service account permissions can't
  be changed after creation, so widening one means making a new one, and the panel says so.
  Rate limits on personal plans are 1,000 reads and 100 writes per hour.
- **Prod: the 1Password app's authorization, never a stored credential.** The desktop app can
  authorize one program at a time: the prompt names the program's path, approval is
  biometric, and it lasts up to 10 idle minutes **for that process only**, so an agent's own
  process doesn't inherit it. Every prod read is gated by the human's Touch ID, enforced by
  1Password. Two caveats, stated:
  - 1Password can't verify a third-party program's identity, so the human is the final check;
  - the official SDKs are Go, JavaScript and Python and maiTerm is Rust, so **a spike must prove
    the path**: `op` as a child process, or a small SDK sidecar, and what 1Password then counts as
    "the process".

**Doppler**
- **A service token per project and config** (`dp.st.<env>.…`): scoped to one config,
  read-only by default.
- Writes (the entry sheet, generate, capture, rotation) use a **separate write-enabled token**,
  held apart and used only inside an approved write.
- **Prod has no per-use human gate**: the token's narrow scope, maiTerm's card policy (§8) and
  Doppler's audit log are the guard. The panel says so, and points to 1Password for a
  biometric gate on prod.

**Infisical (later):** a machine identity scoped to one project and environment.

**Rules for every connection**
1. **Never the human's personal login.** A pasted Doppler token that isn't a service token is
   refused: Doppler warns that a personal or CLI token carries the account's full rights,
   writes included. maiTerm never piggybacks on an `op` sign-in.
2. **maiTerm can mint the credentials.** The human signs in to the provider's CLI once. maiTerm
   runs the mint in its own process (`doppler configs tokens create … --plain`, read from
   stdout), stores the tokens, then signs the CLI out. Otherwise it links to the exact page and
   the human pastes the token into the native sheet.
3. **Scope is checked at connect.** maiTerm lists what the credential can reach, and warns when
   that is wider than chosen: "this token can read 14 configs; you picked 2".
4. **The credential stays inside maiTerm's process where it can.**
   - Doppler and Infisical: Rust calls the REST API with the token in a header (the endpoint is to
     verify). Never `doppler run` with `DOPPLER_TOKEN` in a child's environment, which any
     same-user agent can read with `ps eww`.
   - 1Password: service accounts are used through `op` or the SDKs, so `op` runs as a short-lived
     child with `OP_SERVICE_ACCOUNT_TOKEN` set for one call. That exposure is stated, and it is
     why prod uses the app's authorization instead.
5. **Credentials are stored like secrets.** They go in the OS keychain under maiTerm's own
   service name, Windows entries with local persistence (§5.2). Never in `aiterm-state.json`,
   the phone or a share file.
6. **The provider CLIs are kept away from agents** (§10.3, §10.4):
   - deny `Bash(op *)`, `Bash(doppler *)` and `Bash(infisical *)`, and the CLIs' config (`~/.doppler`);
   - onboarding says don't leave a personal `doppler login` on a machine where agents run;
   - don't use `op` in an agent's tab, since its authorization covers that terminal session and
     sub-shells for 10 idle minutes;
   - **refuse a 1Password prompt you didn't just cause.**
7. **No values cached on disk.** If the provider is unreachable, delivery fails with a sentence.
   Values read for a delivery live in memory for that delivery only.
8. **Don't push where the provider already syncs.** If Doppler syncs a config to Vercel, the
   ledger records the consumer as "synced by Doppler", and maiTerm's own adapter (§7.3) doesn't
   push there too.
9. **What the provider brings that the local vault can't:**
   - **recovery**: a lost machine means revoking maiTerm's credentials in the provider and
     reconnecting;
   - team sharing;
   - the provider's own audit log;
   - its own sync to hosting.

### 5.2 The local vault: the fallback

For people with no secrets manager, or trying the feature out: no account, works offline. It
takes **any tier except prod** (decision 13). **Every key it holds is labelled**, in the panel
and on the entry sheet: *"Kept out of files, backups and agent sessions. An agent running as you
can still read it."*

> **Envelope encryption: one data key in the OS keychain, every value in one encrypted file.**

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

- **One data key**, 256 random bits, under service `{app_data_slug()}.secrets`, user
  `vault-dek`. It is the keychain item alone, which is exactly why the label above is honest
  and why prod values don't go here (§5.3).
- **The local vault and every provider sit behind one `Store` interface**: read, write, list
  versions, plus what each one can do (environments, a runner, sync to targets, versioning,
  a per-use approval, output masking). Delivery, rotation and the panel ask the interface,
  never a particular store.
- **Dev and prod builds get separate entries**, the same split as `accounts/vault.rs` rule 3.
- **Windows: entries are created with local persistence.** keyring's Windows store defaults to
  `CRED_PERSIST_ENTERPRISE`, which **roams** with a roaming domain profile. A data key must
  not leave the machine.
- **The vault file**: `secrets.vault` in the data dir. A versioned header, then one record per
  value, sealed with **XChaCha20-Poly1305**. Each record's associated data is
  `project|env|key|version`, so a record can't be cut out and moved under another name. The
  file is written atomically (temp file, then rename) and is mode 0600 on Unix.
- **Headless Linux, WSL, a box with no Secret Service**: the same rule as `accounts/vault.rs`,
  **no plaintext fallback**. The data key can be passphrase-unlocked (Argon2id) for the session
  instead. Without that the local vault is unavailable and says so, and a provider still works.
- **In memory**: delivery paths hold a value (from either store) only for that delivery, in a
  `Secret` type with a hand-written redacted `Debug`, `zeroize`d on drop. The existing `Secret`
  grows into this. **The scrubber and the leak scanner are the exception** (§10.1, §10.2): they
  keep the values they look for in their own tables while the app runs, and that memory is
  **not** zeroized. For provider-held values, the tables fill as values are read for delivery,
  so a value maiTerm never delivered isn't scanned for. A prod value is read only through
  1Password's app authorization, and its table entry lives until quit.
- **Crates**: `chacha20poly1305`, `argon2`, `zeroize` (RustCrypto, pure Rust, all three
  platforms). No OpenSSL.

**Backups.** A backup of `secrets.vault` alone is useless elsewhere. A backup that also carries
the keychain (a full macOS backup includes `login.keychain-db`) is protected only by the login
password.

**Recovery** (§16 decision 10). Once the local vault holds any value, maiTerm offers a
**recovery key**: 256 random bits shown once as words, never stored on the machine. It wraps a
copy of the data key, kept beside the vault file. Because the key is random rather than chosen,
that copy can't be attacked offline the way a passphrase can. What it does do, stated where it
is made and in §14:
- **anyone who finds it**, an agent included, can open the local vault;
- so the screen says **write it on paper**: a saved PDF or a photo is a file an agent can read;
- **it is never shown in a build with the MCP bridge**, whose webview tools could read it off the
  screen (§6.1).

**It covers only the local vault**: values in 1Password or Doppler are recovered through them
(§5.1 rule 9). If a project's values all live with a provider, no recovery key is needed. Moving
the local vault on purpose is an explicit **Export vault** with a passphrase, in age's format
so the file opens with the `age` CLI too. **Import vault** is the fifth way a value enters (§1).

**Moving from the local vault to a provider** is one action in the panel: connect the provider,
pick the project, and maiTerm copies each value across through the provider's write path, checks
it reads back, then deletes the local copy. The ledger and every target binding stay as they
are.

### 5.3 Not built: an OS-guarded local store for prod

The earlier design (2026-10-22/23, reviewed three times) kept prod values in maiTerm's own store
behind an unlock the OS enforces. **It was dropped for decision 13**, because a provider does the
job better. The research is kept here in case a user ever needs prod without a provider:
- **macOS**: a data-protection keychain item (`kSecUseDataProtectionKeychain`),
  `AccessibleWhenUnlockedThisDeviceOnly` plus a user-presence access control. The keychain
  refuses it without Touch ID or the login password, and the `security` CLI can't reach it. It
  needs an `application-identifier` / `keychain-access-groups` entitlement and a **Developer ID
  provisioning profile** in the release build. Ad-hoc dev builds can't use it. `keyring`'s
  `protected` feature requires presence but only `AccessibleWhenUnlocked`, so the item would go
  through `security-framework` directly.
- **Windows**: HKDF over a Windows Hello signature of a per-install challenge. For an unpackaged
  app the Hello credential **isn't app-bound** (an agent can raise the same prompt), and Windows
  has no hardened runtime, so the unlocked key can be read from maiTerm's memory.
  `UserConsentVerifier` is a prompt that releases no key, and protects nothing.
- **Linux**: only a passphrase.
- **And around it**: a 15-minute idle lock only human actions extend, a migration that deletes the
  passphrase wrap once the OS guard exists, recovery for a key that never leaves the device, and
  keeping leak detection running while locked.

The verdict: a large amount of platform work for a guard that is partial on Windows and absent on
Linux, while 1Password's app authorization gives every platform a per-process biometric gate.

## 6. The agent's side: MCP tools

Every tool here is on `OWN_TAB_ONLY_TOOLS` and `PEER_ADDRESSING_TOOLS`
(`claude_code/server.rs`): it acts for the calling tab on a stated identity, and the cards
name that tab as the agent asking. Tools are listed only while the vault is enabled, like the
browser-link tools.

| Tool | Acts at once? | What it does |
|---|---|---|
| `listSecrets` | yes: read-only | The project's store (provider or local vault), and its keys × environments: set / missing / pending / expiring / overdue, age, `risk`, targets, the latest delivery per target. **Never a value, a length, or a fingerprint.** For a provider, "set" is read from the provider's key names, so a value the human added in 1Password or Doppler shows up. |
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
   approves the request, then settles it**, in that order, before returning. "Stores" means the
   project's store (§5):
   - a 1Password vault through the service account;
   - a prod value through the 1Password app's authorization, so the human approves the write
     with Touch ID;
   - Doppler through its write token;
   - or the local vault, which refuses a prod value.

   The sheet also offers **"I'll set these in 1Password / Doppler myself"**: Done then checks
   that the provider has the names and settles the same way. Rust then runs the
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

### 6.2 First run: bringing existing secrets in

The vault is worth something on day one only if existing secrets can come in. When the vault is
first turned on, maiTerm offers a scan of the **local** folders its tabs and workspaces already
use (an ssh tab's folder is on another machine, so it is skipped) for `.env`-style files
(`.env`, `.env.*`, `*.env`). The scan:
- skips templates: `.example`, `.sample`, `.template`, `.dist`;
- follows no symlinks, skips `node_modules`, `.git` and `vendor`, and stops four levels below
  each folder.

Adopting from a folder **binds that folder to a project explicitly**, on the human's tick (§3:
projects are bound to approved paths). It shows **"Found 14
keys in 6 projects"**, with names, files and a guessed tier (`.env.production` → prod, a
`sk_test_` value → dev), and **no values**. Adopting is `adoptSecrets` in bulk:
- one sheet;
- the human ticks what comes in and confirms each tier;
- Rust reads the files;
- each file's lines become references on the human's say-so (§7.4's rules then apply to it).

Nothing is scanned without the human starting it, and an agent can't start it: the scan reads
files across projects, which is wider than any one agent's project (§3).

## 7. Delivery: getting a value where it needs to be

Every delivery reads its values from the project's store at that moment (§5): nothing is
copied out of a provider ahead of time. Every delivery path follows the same four rules:
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
  `GetNamedPipeClientProcessId` (Windows). The peer's argv and cwd are read from the OS, and the
  values are released only to that pid.
- **The redeemer must be running in that service's terminal.** The genuine shim can be run by
  anyone, and the ticket sits in the service's scrollback and `getServiceOutput`. So an agent
  could type `cd <folder> && <shim> --ticket <t> -- sh -c '<cmd>'` in its own tab within 60
  seconds and pass a signature, argv and cwd check. So the peer's **controlling terminal must
  be the service's PTY**, which maiTerm owns: `proc_pidinfo` `e_tdev` on macOS, `tty_nr` in
  `/proc/<pid>/stat` on Linux. On Windows, ConPTY gives no such field, so the check walks the
  peer's parent chain to the service shell's pid; that one is weaker, and the doc says so.
  Agents run in their own tabs, not in service consoles, so this check is the one that blocks
  them.
- **And it must be maiTerm's genuine shim, checked so it can't be raced:**
  - macOS: identify the peer by **audit token** (`LOCAL_PEERTOKEN` plus
    `kSecGuestAttributeAudit`), not by pid, which can be reused. The shim is built with the
    **hardened runtime and library validation**, so `DYLD_INSERT_LIBRARIES` can't put an
    agent's code inside a validly signed process.
  - Windows: verify the image of the **running process** through its handle, not the file at
    its path (a running exe can be renamed and another file swapped in).
  - Linux, which has no platform signature: hash `/proc/<pid>/exe`, not the path on disk.
  - Dev builds are ad-hoc signed and use the hash check.
- **What is still open, stated:** a process that connects, forks a child holding a copy of the
  socket and then execs the genuine shim passes every check, and the child reads the values. On
  Linux, ptrace depends on `ptrace_scope`. Both are §1's same-user limit.
- **On Unix** the shim `exec`s `sh -c`, so the process tree is exactly what it would be without
  it. **On Windows** there is no `exec`: the shim spawns the child (through `cmd /c`), forwards
  Ctrl-C/Ctrl-Break and returns the child's exit code.
- **On a TTY** (an interactive dev server) the shim can't mask without breaking the terminal,
  so masking is off there. That output still reaches agents only through the scrubbed MCP
  boundary (§10.1).

**Secrets that are files** (`kind: "file"`, §4.2). A service-account JSON, an Apple `.p8` or
`.p12`, a TLS or SSH key: most of these are read from a path, not from a variable.

- **Entry and encoding.** A file key is entered on the sheet by choosing the file, which Rust
  reads, not by pasting: a `.p12` is binary. It is stored as bytes. A target that takes text (a
  GitHub secret) gets base64, and the card says so.
- **Where it lands.** A fresh per-run directory under one runtime root:
  - Linux: `$XDG_RUNTIME_DIR` (memory-backed), falling back to a 0700 directory under maiTerm's
    data dir when it is unset (ssh, WSL, headless);
  - macOS: the per-user temp directory, which **is on disk**, and the doc says so;
  - Windows: a directory with an explicit DACL for the user only, since mode bits mean nothing
    there.

  Mode 0700/0600 on Unix. `path_var` is set to the file's path.
- **Cleanup is maiTerm's, not the runner's.** On Unix the shim `exec`s, so nothing of it is left
  to clean up. maiTerm watches the run (kqueue `NOTE_EXIT` on macOS, a pidfd on Linux, a process
  handle on Windows), and deletes the directory once the **whole process group** has exited.
  Services fork children that may read the path later. A crash can leave a directory behind, so
  maiTerm sweeps the runtime root at startup and every hour.
- **The runtime root is denied like the vault file**: in the `PreToolUse` deny (§10.3), the
  user-scope read-deny rules (§10.4) and the sandbox config. The ledger records the run, not a
  lasting file, so a per-file entry would always be late.
- **Masking knows structure.** A whole-value match misses a JSON key printed in part or
  re-serialized, or a PEM printed line by line. So for a file key the masker also matches each
  string field of a JSON value of 16 characters or more (`private_key`, `client_secret`), and
  each line of a PEM body.
- **A one-shot reader can take a pipe** (`/dev/fd/N`) instead of a file, which leaves nothing on
  disk. It is offered where the consumer accepts one.

Rotation needs nothing extra, since each run gets the current version.

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
- **A service bound to a prod-tier environment is off by default**, like a prod `run` (§8): the
  human turns it on per service, with a card. Once on, starting and restarting it are at once.
- **An agent's `startService` / `restartService` never raises a 1Password prompt.** A prompt the
  human didn't cause trains them to approve prompts. The service waits instead (below), and the
  human authorizes from the panel or the notice.
- **A service that needs 1Password prod values waits for the human's authorization.** After a
  relaunch, `was_running` services start on their own (docs/stack.md §5), and an overnight
  restart or a deploy with the screens off can't do Touch ID. Such a service shows **waiting
  for 1Password** rather than crashed, sends one notice (gated by mute), and starts when the
  human authorizes. Services fed by Doppler or by dev/staging values start as usual. The state
  is transient, like all service status, and never persisted.
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
**Better than a file, where available: a `.env` that is never on disk.**
- For a 1Password-backed project on Mac or Linux, 1Password's own mounted `.env` (Environments)
  does this, and maiTerm points the target at it instead of writing.
- Otherwise, on Mac and Linux, maiTerm can serve the path as a named pipe that hands the rendered
  values to each reader and holds nothing at rest. The cost: a framework that reads the file
  twice, or only checks that it exists, needs testing per framework.

A plain file stays the last resort.

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
| a stack service bound to the environment (§7.2) | at once | at once | **off**. Enabled per service with a card, then starts at once (waiting for 1Password's authorization if needed) |
| `generateSecret` (missing value) | at once | at once | a card |
| `captureSecret` | a card the first time per command + place + `extract` | same | a card each time |
| where values may live (§5) | provider, or the labelled local vault | same as dev | **a provider only** |
| reveal / copy in the panel | yes | yes | **no**: open it in the provider's own app |
| leak found | reported to the human | reported, rotation proposed | reported, **rotation proposed as a Decision** |

A prod card can be approved on the phone (§12): it names what moves and where, never a value.

## 9. Rotation

### 9.1 A rotation run

A run rotates one or more keys in one environment as a single plan. **With a provider, the
provider stores the versions and maiTerm drives the consumer side**: every consumer, the
overlap, the verification and the revoke card. A pending value is held where the provider
**does not sync it onward**: a separate Doppler config that has no integrations, or a separate
1Password item. Never a reserved name in the live config, which Doppler's own sync would push
to Vercel along with everything else. Exactly how is designed in the rotation phase, under that
constraint. It follows the AWS staging-label model:

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
The scanner holds the values in memory (not zeroized; for provider-held values, only those
maiTerm has read for a delivery, §5.2) and runs
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
- **Each runtime's own read-deny rules** for the vault file and the ledger's files, written by
  maiTerm into every runtime it supports (§10.4, §16 decision 6). Unlike the hook, they hold
  when maiTerm is down.
- **The provider CLIs are denied to agents** (§5.1 rule 6): `Bash(op *)`, `Bash(doppler *)`,
  `Bash(infisical *)` and the CLIs' config folders, in the hook and in each runtime's read-deny
  rules (§10.4). Without this, a provider is just a faster way for an agent to read everything:
  a signed-in `doppler` hands out any value without a prompt, and `op` does for 10 minutes after
  the human has used it in that terminal.
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

### 10.4 Read-deny rules in every runtime, written with precision

Darryl (2026-10-23): yes, "so long as it's done with precision so maiTerm doesn't have any
chance of corrupting settings", and not only for Claude. Agent tabs run Claude, Codex, Gemini
and Grok. maiTerm's own integration is deepest for Claude: `accounts/mod.rs` says "Claude is
the only runtime implemented", and Grok is a placeholder there. So each runtime below gets its
writer only once its row is verified. The rows were read from each runtime's docs and source on
2026-10-23 and corrected by a review the same day.

**Two scopes.** The vault file and the file-key runtime directory (§7.1) are global, so their
rules go at **user scope**. A ledger file (a `.env` maiTerm wrote) is in one project, so its
rule goes at **project scope**.

| Runtime | Mechanism | What maiTerm writes | Strength |
|---|---|---|---|
| **Claude Code** | `permissions.deny` `Read(<path>)`. Covers the built-in file tools, the file commands Claude recognizes in Bash (`cat`, `head`, `tail`, `sed`, `tee`) and redirection targets; misses reads that don't name the file (`grep -r`, a script). **Path syntax:** a single leading `/` anchors at the settings file's source, not the filesystem root, so **absolute paths must be written `//Users/…`** (Windows: `//c/…`). Settings are **strict JSON**: a comment is a settings error, and Claude can drop the whole file | Project: rules in `.claude/settings.local.json` **at the location Claude itself uses**: the git repository root even when started in a subdirectory, the **main checkout's** root in a worktree, the cwd's `.claude/` outside git, when the repo root is `$HOME`, on Windows, or when ownership doesn't match (versions before 2.1.211 differ). User scope: `~/.claude/settings.json` | Good. Enforced by Claude, not by maiTerm |
| **Grok Build** | Native allow/ask/deny rules, in which **a single `/` is absolute** (its docs: `Read(/Users/you/private/**)`). It also reads Claude's settings files and translates their `permissions.deny`, so one string could mean two things | **Native rules** in `.grok/config.toml` (project) and `~/.grok/config.toml` (user), rather than relying on the translation. The behavioural check below also confirms that the `//` rules in the Claude file don't misfire under Grok | Good, once verified |
| **Gemini CLI** | The **policy engine**: TOML rules (`toolName`, `argsPattern`, `decision = "deny"`) in `~/.gemini/policies/`. A parse error is **isolated per file**, so a broken maiTerm file can't disable the human's other policies. A user-tier deny beats YOLO, whose allow-all sits in the default tier (`policies/yolo.toml`). But "Always allow" answers are saved to `~/.gemini/policies/auto-saved.toml` at priority 950, inside the same user tier. `argsPattern` is a regex over the raw arguments, with no path normalization: `..`, a relative path or a symlink can get past it. `.geminiignore` is filtering, not a boundary | **A file maiTerm owns outright**: `~/.gemini/policies/maiterm-vault[-dev].toml`, every rule at **priority 999** (the maximum) so a saved "Always allow" can't outrank it. Rules for `read_file`, `read_many_files` and the grep and glob tools, each with absolute and `~` forms, plus `run_shell_command` prefix rules for the common readers | Low to medium: pattern-based and bypassable |
| **Codex CLI** | Filesystem permission profiles (`[permissions.<name>.filesystem]`, `deny`), **beta**, which **apply only to sandboxed tool calls**: under `danger-full-access` or YOLO a deny does nothing. The active profile is the human's choice (`default_permissions`). Project `.codex/config.toml` loads only for trusted projects | **Nothing until a test settles it.** maiTerm must not change which profile a human runs under. The candidate is a project-level profile layered on the human's, offered on a card that names the change | Unknown |

**Gemini CLI's own future.** It stopped serving Google AI Pro, Ultra and free users on
2026-06-18, and those users were moved to the closed-source **Antigravity CLI** (`agy`). It
remains available with paid Gemini API keys and enterprise licences. Antigravity's mechanism is
unknown, so it gets nothing until it is looked at.

**A behavioural check, per runtime version.** Re-reading the file proves only that the bytes are
right, not that the runtime reads the rule as meant: a rule anchored wrong parses perfectly and
protects nothing. So for each runtime version maiTerm sees, a check starts the runtime
non-interactively in a scratch HOME against a scratch file under the rule, and confirms the read
is refused. A runtime whose check fails or can't run gets no rules, and the panel says so. That
check is also Grok's "test proves it" step.

**What "precision" means here.** These rules come from the existing writers (the MCP entries in
`mcp_proxy/entries.rs`), tightened where those fall short:

1. **Own a file outright wherever the runtime allows it** (Gemini's policies directory). A file
   only maiTerm writes can't corrupt anyone else's settings.
2. **Keep two sets: the rules that are wanted, and the rules maiTerm added.** JSON arrays can't
   carry a marker, so both sets are kept per file, in maiTerm's data dir.
   - A wanted rule already present, written by the human, counts as satisfied but is never
     treated as maiTerm's.
   - If a wanted rule goes missing (including the human's own copy), maiTerm adds its own,
     records it as added, and the panel shows **"re-added"**.
   - A human who wants a project's rules gone uses the panel's **"Stop protecting this project"**
     switch, so a deliberate removal sticks instead of being re-added forever.
   - maiTerm removes only rules in its added set, one entry for each one it added, never a rule
     it didn't write.
3. **Change only the bytes that must change.** TOML through `toml_edit`. JSON by a **surgical
   splice of the `permissions.deny` array's text**, not a parse and re-serialize: today's writer
   (`edit_json` in `mcp_proxy/entries.rs`) re-sorts keys, because `serde_json` is built without
   `preserve_order`. Turning that feature on is crate-wide and would change every map in the
   app. Re-serializing also changes indentation, CRLF, the trailing newline and number spelling
   (`1e3` becomes `1000.0`). The check is **byte equality outside the splice**.
4. **A file that doesn't parse is left alone**, and the panel says why. Claude settings are
   strict JSON, so a file with a comment is in this case: maiTerm never "repairs" a file.
5. **No backup in the project.** A backup beside the file would match no ignore rule, get
   committed by a `git add -A`, and carry the human's local settings (an `env` block can hold
   tokens). Rollback (rule 8) uses the exact bytes maiTerm read in rule 7. A copy for the human,
   if kept at all, goes in maiTerm's data dir, mode 0600, and the newest one only.
6. **Write atomically, under a lock that spans processes, and follow symlinks only to expected
   places.**
   - The temp file goes in the resolved target's directory, so the rename can't cross a
     filesystem.
   - The lock is an advisory file lock (`flock` / `LockFileEx`): the dev and installed builds
     write the same files, and the `WRITE` mutex in `entries.rs` is process-local.
   - Account roots symlink the **user-level** `~/.claude/settings*.json` (docs/login.md §5.4), so
     those links are followed to their known targets.
   - **In a project, a symlinked `.claude` or settings file is refused**: the project is
     agent-writable, and following an agent's symlink would let it aim maiTerm's write at any
     JSON file. (Claude itself treats a symlinked `.claude` as repository-supplied.)
   - `entries.rs` renames over symlinks today (`std::fs::rename(&tmp, path)`), which would break a
     dotfiles-symlinked `~/.claude.json` or `~/.codex/config.toml`. That is fixed in the same work.
7. **Compare before writing**: if the file's hash changed between maiTerm's read and its write,
   re-read and redo the edit. A small window between the compare and the rename remains, and the
   doc says so: a concurrent save in that window is caught by rule 8.
8. **Verify after writing**: the bytes outside the splice are identical, it parses, and the rule
   is there. On a mismatch, restore the bytes read in rule 7, **only if the file still holds
   exactly what maiTerm wrote**; otherwise leave it and report it. Then the behavioural check
   above, once per runtime version.
9. **Keep the rules out of git, or don't write them.**
   - If `.claude/settings.local.json` is **tracked**, refuse, and the panel says why: an exclude
     does nothing for a tracked file, and maiTerm's absolute paths would be committed.
   - Otherwise, if `git check-ignore` says it isn't ignored (Claude adds a global exclude only
     when *Claude* creates the file), add it to the path `git rev-parse --git-path info/exclude`
     gives. That is local, and works in worktrees, where `.git` is a file. Never edit `.gitignore`.
10. **Dev and prod flavours don't touch each other's rules.** Gemini's file name and the rule
    records carry `app_data_slug()`.
11. **Remove the rules when they stop being true**: when a ledger file is deleted and when the
    vault is turned off. macOS has no uninstall hook, so the panel has **"Remove maiTerm's rules
    everywhere"**, and turning the vault off offers it. A drift check reasserts the wanted set
    like the MCP entries' 30-second loop.
12. **Test with a scratch HOME**, against real files from each runtime (memory: "Reviewers must
    isolate HOME"). Cases: strict-JSON errors (left alone), CRLF, worktrees, a symlinked
    `.claude` (refused), a tracked local file (refused), dotfile-symlinked user files.

## 11. UI

### 11.1 The Secrets panel

A full-window view per project, like Preferences, opened from the workspace's context menu, a
sidebar row and every secrets card. **No shortcut in v1, and not a new `tab_type`** (that would
add a value an older build can't load; §16 decision 8). It needs the width, so it is not a side
dock:
- **the project's store** at the top: "1Password · maiTerm · api" or "Doppler · api", or the
  local vault with its label (§5.2), plus a **Connect a provider** action and **Move to a
  provider** for a project still on the local vault;
- **a matrix of keys × environments**, each cell showing set (with age), missing (red when
  required), pending (a rotation in progress), overdue, or expiring;
- expanding a key shows its description (quoted as the agent's), provider, template, targets,
  version history (no values), consumers from the ledger, and its audit rows;
- actions: Set, Generate, Rotate, Remove from target, Reveal, Copy;
- **unused keys**: each key shows when it was **last delivered** (from the ledger). Delivery is
  not use: a key pushed once to GitHub Actions may be used by CI every day. So only a key with
  **no live target binding** and no delivery for 90 days is flagged "unused?". Such a key is
  held only for local runs, services and `.env` files, none of which touched it. The panel
  suggests removing it from the vault, never from a target. Unused keys are the ones nobody
  remembers to rotate. The ledger can't see a key the human copied somewhere by hand, so this is
  only ever a suggestion.

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

**No agent approves a secrets card, the Overlord included.** Only `decide`, reached by the
human's click or a paired phone, moves a request out of pending (docs/agent-requests.md §2).
With `overlord_answers_escalations` on, the Overlord agent answers escalations, and a secrets
card must never be one of them. **One exception exists today: a dev build.** There
`decide_agent_request` is a Tauri command, and `tauri-plugin-mcp-bridge` lets an agent call the
webview, so an agent could approve its own card. So, as with prod entry (§6.1), **a build with
the bridge refuses desktop approval of prod-tier secrets cards** (the phone can still approve
them). Tests cover both rules, so a later change can't loosen them.

**A card lists what reached the agent since the human last typed in its tab.** An agent can be
steered by a peer, or by other input maiTerm delivers, into asking to deploy prod. The card then
names the agent that filed it, which is true but hides who started it. maiTerm records every
input it types into a tab: mesh and bridge messages, comms pickups, board notices, follow-ups,
`driveTab` directives. It also knows when the human last typed there (`Turn.typedBy`). A secrets
card lists **every maiTerm-delivered input since the human last typed in this tab**, by source:
"after a message from 'Release', a follow-up and a board notice". There is no time window to wait
out. Those are maiTerm's words, built from its own record, never from the message text.

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
- **A prod approval from the phone, when the prod values are in 1Password**, still needs
  1Password's authorization at the desktop before it can read them. It waits for that, and the
  phone card says so: "runs when you next approve 1Password on the desktop". Approved requests
  normally fail after 5 minutes unreported (`APPROVED_TTL_MS`), so these kinds get the same
  per-kind TTL treatment as §6.1. A rotation's prod steps (redeploys, restarts, a revoke recipe
  that needs the old value) wait the same way. Doppler-backed prod has no such wait.
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
| **A determined agent running as the same OS user** | Prod values live only with a provider, and with 1Password every prod read needs the human's per-process authorization (§5.1); provider CLIs denied to agents (§10.3, §10.4); the `PreToolUse` speed bump; no oracles (§1 rule 4) | Values in the local vault, and maiTerm's stored provider credentials (a 1Password service account, Doppler tokens): the keychain item is readable to it on Linux and Windows. Doppler prod tokens get no per-use gate. A 1Password prompt the human approves on reflex. A personal provider login the human left on the machine. Any process environment after delivery. **A recovery key it finds** opens the local vault. **The real boundary is a configured sandbox (§10.3) or a separate OS user** |
| **Another local process as the same user** | As above | As above, the same reach docs/login.md §9.2 accepts |
| **A stolen or lost machine** | Provider values: revoke maiTerm's credentials in the provider and reconnect (§5.1 rule 9) | Local-vault values, if the thief also has the login password |
| **A stolen backup or synced data dir** | The vault file alone is useless; Windows entries don't roam (§5.2); provider values were never on disk | A backup that also carries the macOS login keychain opens the local vault to whoever has the login password |
| **A lost or compromised phone** | Values never reach the phone; approvals need a paired device token (docs/agent-requests.md §2) | A paired phone can approve a prod deploy, so unpairing is the response |
| **A shared screen / shoulder** | Masked by default; reveal times out; copy stays out of clipboard history | — |
| **maiTerm's own logs and errors** | `Secret` has no `Display`/`Serialize`; errors name the operation and key, never the value (the `accounts/vault.rs` rule 1); adapter errors are fixed sentences (§7.3) | — |
| **An agent in a dev build** | Prod value entry refused while the MCP bridge is loaded (§6.1) | Dev and staging entry can be watched through the bridge's webview tools |
| **A malicious dependency in the app being run** | — | It gets whatever the process gets. That is true of every secrets manager |

**The vault keeps secrets out of sessions and decides where they go. It does not make the code
that uses them trustworthy, and it does not contain an agent that shares the human's OS
user.** Every card on a `run` or prod path says so in one line.

## 15. Rejected

- **One keychain item per secret**: §5.2.
- **The tier deciding how well a key is stored** (2026-10-28): a "dev" key is often live. The tier
  decides routing and cards; protection is the same for every key (§5).
- **maiTerm's own OS-guarded store for prod** (2026-10-28): §5.3. A provider does it better.
- **Using the human's own provider login** (`doppler login`, an `op` sign-in): it reaches
  everything they can, and any agent running as them can use it too (§5.1).
- **Giving the agent the value and asking it not to print it**: it ends up in the transcript,
  so with the model provider, the moment the agent reads it.
- **Typing `export`s into a PTY**: scrollback, history and the scrollback database (§7.2).
- **Handing agents a run ticket** (the first draft's `maiterm run --ticket` for agents): a ticket
  is redeemable by whoever holds it, the binding was self-reported, and the agent could skip the
  masking (§7.1).
- **Trusting a CLI's own config for the destination**: the agent can edit it (§7.3).
- **A `curl`-style "call this URL with the key" tool**: every exfiltration needs exactly that.
  A later "signed request" tool would be acceptable only with a per-key host allowlist, set by
  a card. **Reconsidered 2026-10-28** after 1Password's OpenShell gateway (§2), which does
  exactly that: the agent holds a placeholder, and the gateway swaps the real value in only for
  approved hosts and a set time window. Still not in v1; a candidate after leak detection.
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
corrected the facts under decisions 2, 6 and 7. Decision 6's reason turned out to be wrong, and
Darryl re-decided it the same day.

1. **Phone value entry: not in v1; later, end-to-end encrypted** (§12). The need is real only
   once rotation exists (pasting a newly minted key from a mobile dashboard or a password
   manager). Sealing the value to a one-time key on the desktop card protects the value; the
   card's public key is protected by maiLink's pinned TLS, which the relay never carries.
2. **Superseded by decision 13** (2026-10-28). It was an OS-enforced unlock for prod values in
   maiTerm's own store. The research is kept in §5.3.
3. **Superseded by decision 11** (2026-10-28). It was "1Password as a second backend, built
   after targets". Providers are now the first-class store.
4. **Targets: GitHub Actions, Cloudflare, Railway; then Vercel, Fly; AWS and Kubernetes
   later** (§7.3). **Provider packs: Stripe, GitHub, Anthropic, OpenAI, Cloudflare, AWS** (§9.5).
   GitHub is near-universal, maiTerm itself deploys to Cloudflare, Railway is in use here, and
   AWS/Kubernetes are the hardest adapters and most likely to have their own secrets manager.
5. **Prod `run`: off by default; enabled per project, then a card per run** (§8). Prod
   credentials on a laptop are the anti-pattern this ends; prod migrations belong in the deploy
   pipeline; the rare one-off ops job fits a card per run.
6. **Read-deny rules: yes, for every runtime, written with precision** (§10.4; re-decided
   2026-10-23). The first version said no, because a Claude rule "covers only the Read tool".
   That was false: it also covers `cat`-style Bash commands and redirections. These rules hold
   even when maiTerm is down, unlike the hook. Darryl's conditions were "no chance of
   corrupting settings", which became §10.4's twelve rules, and "think more broadly beyond
   Claude", which covers Codex, Gemini and Grok. Each runtime's writer ships only after its row
   in §10.4 is verified; Codex waits on a test.
7. **The sandbox is offered as one click, after a spike proves it** (§10.3). Out of the box the
   sandbox protects none of this, so the offer writes a specific config (credentials or
   `denyRead` entries plus `Read` deny rules for the vault file and ledger files). The spike proves
   that config blocks the vault file, the ledger files, the keychain and other processes'
   environments. It ships only if the spike passes, since otherwise the button would promise
   protection it doesn't give. Native Windows gets no offer: the sandbox runs only on macOS,
   Linux and WSL2. Following decision 6, the spike covers Codex's and Gemini's sandboxes too,
   so the offer is made per runtime where one proves out.
8. **The panel is a full-window view like Preferences, with no shortcut and no new `tab_type`**
   (§11.1). The keys × environments grid needs width, it is an occasional screen mostly reached
   from a card, and a new `tab_type` value is the older-build-loads-empty risk for no gain.
9. **Dynamic secrets: advice, not a feature.** When a long-lived `AKIA…` key is stored, the panel
   suggests AWS SSO (`aws sso login` already yields short-lived credentials) or OIDC for CI, as
   the OIDC advisor does (§7.3). The providers that support short-lived credentials ship the
   tooling; maiTerm's job is to stop the long-lived key being stored, not to reimplement STS.
   (The SSO token cached in `~/.aws/sso/cache` is itself a bearer credential an agent can read;
   the advice says so.)

10. **A recovery key, for the local vault only** (§5.2 "Recovery"). Darryl: yes, "but it
    depends. If we are integrating with another password manager, then I'd say no, as the
    recovery is tied to that password manager." So it is offered when the local vault holds
    values, and never for values held by a provider, whose recovery is the provider's. It is 256
    random bits, so unlike a passphrase it can't be guessed offline. It is never stored on the
    machine.

**Added 2026-10-28**, after Darryl asked whether 1Password had moved toward Doppler and whether
maiTerm should lean on them instead of its own vault:

11. **Providers first** (§5, §5.1). Values live in 1Password or Doppler whenever one is connected;
    Infisical later. maiTerm builds the integration layer around them: "it basically is adding a
    much better integration layer on to Doppler and 1Password". maiTerm always uses **its own
    narrow credentials** (a 1Password service account, Doppler service tokens), never the
    human's login, and the provider CLIs are kept away from agents.
12. **The same protection for every key** (§5). Darryl: "why would dev/staging need less
    security? Oftentimes those would still be functional keys even if labeled as dev." Every key
    is `live` unless it provably can't do harm. The tier decides routing and cards, not storage.
13. **The local vault is a labelled fallback, and prod needs a provider** (§5.2, §5.3). The local
    vault holds any non-prod key for people without a provider, with a label saying plainly what
    it does and doesn't protect. Prod values need a provider, and through 1Password every prod
    read is gated by the human's Touch ID. That drops the OS-guarded local prod store, the
    heaviest and weakest part of the earlier design.

### 16.1 Added 2026-10-23: considerations raised after the decisions

Darryl asked what else should be asked. Each item was then written into its section:
- recovery (§5, decision 10);
- secrets that are files (§4.2, §7.1);
- services waiting on the human's 1Password authorization after a restart (§7.2);
- checking the shim's code signature, not its path (§7.1);
- the Overlord and mesh peers never approving, and cards naming a mesh origin (§11.2);
- unused keys (§11.1);
- bringing existing secrets in (§6.2);
- what shipping a secrets manager asks of maiTerm itself (§18).

Free or paid is Darryl's product call and doesn't change the design.

## 17. Build order

1. **Core and the provider spikes.**
   - **First, two spikes**: driving 1Password's app authorization from maiTerm (a Rust app; `op`
     as a child, or a small SDK sidecar), and the Doppler REST endpoint for reading a config with
     a service token (§5.1).
   - `src-tauri/src/secrets/`: the `Store` interface; the data model with `risk`; the Secrets
     panel as a full-window view, with the store and the local vault's label shown; environments;
     the audit log.
   - **Doppler and 1Password connections**: minting or pasting maiTerm's own credentials, the
     token-type refusal, the scope check, keychain storage. Dedicated 1Password vaults.
   - **The local vault**: one data key, local-only Windows persistence, the passphrase for
     boxes with no keychain, the recovery key, moving the vault to a provider (§5.2).
   - `accounts/vault.rs`'s `Secret` grows `zeroize` and moves to a shared module.
2. **The agent manifest.** `listSecrets`, `declareSecrets` with its locked fields,
   `requestSecretValues`, the entry sheet writing to the project's store (or "I'll set these
   myself"), the combined store-approve-settle Done, per-kind request TTLs and the reload remap
   (§6.1), the phone refusal and `desktopOnly` (§12), the ⟦SECRETS⟧ notice, `generateSecret`, the
   "no agent approves" test and the mesh-origin line on cards (§11.2). **This is the first phase an
   agent can use, and it proves Darryl's flow end to end.**
3. **Keeping agents off the provider, before anything delivers.** The provider-CLI deny rules
   (`op`, `doppler`, `infisical`, their config folders) and onboarding's warnings (§5.1 rule 6).
   The sandbox spike (§10.3), and the one-click offer only if it passes.
4. **Local delivery.** The scrubber at every MCP and persistence boundary (§10.1) **first**,
   then `runWithSecrets` with masking and file keys, the `maiterm-run` shim with peer-verified,
   **signature-checked** tickets for stack services, and "waiting for 1Password" (§7.1, §7.2),
   `writeEnvFile`, `adoptSecrets` with the first-run scan (§6.2), and with them **the
   ledger-file `PreToolUse` deny and the read-deny writers** (§10.3, §10.4), each runtime only
   after its behavioural check passes: Claude first, then Grok's native rules; Gemini CLI
   (paid-API and enterprise users only since 2026-06-18) after those; Codex once its test settles
   the approach. Antigravity CLI waits to be looked at. The `entries.rs` symlink fix goes in
   with them.
5. **Targets.** `declareTargets`, `deploySecrets`, the ledger, adapters with pinned addresses and
   binaries (§7.3): GitHub Actions, Cloudflare, Railway. The OIDC / AWS SSO advice, which is
   about targets. "Synced by the provider" consumers, so maiTerm doesn't push where Doppler
   already syncs (§5.1 rule 8). "Last delivered" and the unused-key flag (§11.1). **The end of
   v1**, followed by §18 before it is advertised.
6. **Rotation.** Runs with human mint and revoke, `requestRotation`, the run view, age and
   expiry proposals, the first provider packs (§9.5), end-to-end encrypted value entry on the
   phone (§12).
7. **Leak detection.** The dedicated transcript scanner, scrollback and pre-commit scans,
   leak-triggered rotation, the rest of the `PreToolUse` deny (keychain CLIs, process
   environments).
8. **Automation and reach.** Capture recipes with their oracle guards, adapter mint/revoke, ssh
   delivery, Infisical, Vercel and Fly, then AWS and Kubernetes; a signed-request gateway in the
   OpenShell style (§15).

Each phase is reviewed before the next. The shim, the adapters and the read-deny writers must be
reviewed with a scratch HOME. **Two tests must exist before the first adapter ships**: no value
in argv, and no change to an agent-writable file or PATH entry moves where a push goes.

## 18. Before it ships: what a secrets manager asks of maiTerm itself

Shipping a secrets manager raises the bar for the whole product. Before the vault is
advertised on maiterm.dev:
- **An outside security review of phases 1–5** (v1), with the threat model (§14) as its brief.
- **A `SECURITY.md`** with a way to report vulnerabilities privately, and a stated response time.
- **Nothing identifying about the vault leaves the machine.** No key name, project name, count
  or event reaches the updater's usage count (memory: "Usage stats / MAU"). The one deliberate
  exception is the phone doorbell (§12): a push travels through the relay and APNs carrying a
  kind and a title (docs/mailink-protocol.md §6). So secrets pushes carry only a **generic
  title** ("A secret needs your attention"), never a key, project or provider name. The phone
  fetches the details over maiLink's pinned LAN connection.
- **The website copy stays true** (website/CLAUDE.md): it says what §1 and §14 say, including
  that the vault does not contain an agent running as the same OS user, and that prod protection
  differs by platform. It makes no "military-grade"-style claims.
- **Free or paid** is Darryl's call, and doesn't change the design.
