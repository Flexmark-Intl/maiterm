# maiLink — Mobile Companion Protocol & Architecture

> Status: **design / contract draft v0.3**. This is the shared contract between the
> maiTerm **desktop** side (this repo) and the **maiLink mobile app** (separate codebase,
> built collaboratively with the maiLink agent). Date: 2026-06-30.
>
> **v0.8 changelog** (2026-09-08). Additive: `tool` and `detail` on `Chat`, `ChatDetail` and every
> `chat_state` frame — the agent's currently-running tool and its primary argument. maiTerm has
> tracked both on every PreToolUse since the hooks went in, and cleared both on Stop, but only ever
> emitted them nested inside a *permission* prompt view; a client reading them at top level was
> reading a field with no producer (measured: 354 frames, including a tab actively running tools,
> not one carried either). Both are explicitly `null` when nothing is running, per the merge rule
> below — a turn ending has to CLEAR the status line, and an omitted field would leave the last
> tool of the last turn pinned under an idle agent. The live half needed its own diff: these move
> within a turn while `state` sits at `active`, so the attention key structurally cannot see them,
> and a tool-only change deliberately skips frame enrichment (a tab that just started a tool is
> active this instant, so the frame's own `now` is honest and two transcript tail reads are not).
>
> **v0.7 changelog** (2026-09-08). Additive: `ChatDetail.subagents` + the WS `subagents` event
> (§4.3 `Subagent`). Delegations were invisible — one static tool chip at launch and nothing after
> — so a thread whose agent was five minutes into a code review said only "working…", and the only
> way to see why was to open the desktop and read the terminal. Modelled on `shells` because that
> shape works, with two differences that matter. It covers SSH tabs: a subagent isn't a process
> needing the local process table, its lifecycle is written to the parent transcript, and the
> mirror shadows that — such a tab loses only `lastLine`. And **the lifecycle is not
> tool_use→tool_result**: the `Agent` tool acks in ~2s with an agentId, and the real outcome lands
> later as a `<task-notification>` turn, so pairing use with result would mark every delegation
> done two seconds after it started. Keyed by agentId, never the tool_use id.
>
> **v0.5 changelog** (2026-09-06, built with the maiLink agent). **One BREAKING change**, in one
> go, by the product owner's decision: `tasks` — on `ChatDetail` and the WS event — is now
> maiTerm's own task board (`MaitermTask`), not Claude Code's session board (`AgentTask`, gone).
> docs/tasks.md had already demoted that board to an importer INPUT: maiTerm's `Workspace.tasks`
> is the source of truth and the importer folds the Claude board into it, so for a Claude tab both
> pipes carried the same work — serving both would have shown it twice and left the phone to pick.
> Rows are richer (seven lanes, workstreams, assignee, origin, dependencies) and now come from any
> runtime and from tabs with no live session. New `GET /tasks` serves the whole board. Two rules
> came out of the peer's first implementation and are stated once under `MaitermTask`: render from
> `effectiveStatus`, count from `status`; derive nothing from `blockedBy`. `effectiveStatus` exists
> because the desktop's "blocked" is DERIVED at render time from information the phone never
> receives — the field crosses the seam where a note could not. The WS change key hashes what rows
> render AS (names, effective status), not their own fields, after a review showed a rename or a
> cross-tab blocker finishing would otherwise never reach a connected phone. `GET /tasks` honours
> designation (§7) exactly as the chat does — an excluded tab's rows and name never appear. The
> Overlord surface (§13) is designed and NOT yet on the wire; this release is the tasks half.
>
> **v0.4 changelog** (2026-09-01, built with the maiLink agent). Three additions and one rule
> that outranks them.
>
> **The rule — a field's ABSENCE is never a claim.** Four bugs in three days came from one shape:
> a value that is correct in the component that defines it, read by a consumer that means
> something else by it. `chat_state` frames are MERGED over rows built by `GET /chats`, so an
> omitted field is indistinguishable from a field claimed empty — every field on a frame is now
> always present, carrying an explicit `null`, with `meta` the single documented exception.
> `state:"idle"` no longer implies a turn finished (a starting session registers as idle), so
> `unread` and both attention announcers were rebuilt on facts rather than on that word. See the
> notes under `state`, `unread` and `chat_state`; they are the load-bearing part of this release.
>
> **Files (`sendFilesToPhone` + `GET /assets`, `GET /assets/{id}`).** An agent sends files to the
> paired phone; they appear as a `kind:"asset"` transcript turn and in a cross-chat Files view.
> `FileAsset` carries `available` because a 404 cannot distinguish an eviction from a broken
> server. maiTerm decodes nothing — no dimensions, duration or thumbnails, and no thumbnail
> endpoint — since the phone derives all of it from a downloaded file for free.
>
> **Models (`GET /models`).** The picker stops hardcoding a list that went stale the day Fable 5.1
> shipped. Each row says whether it came from this account's server-pushed cache or maiTerm's
> curated tiers, because those are not equally trustworthy. No `confirm` field: whether `/model`
> pops a confirmation is a TUI fact maiTerm cannot observe, and the Fable gate turned out to be a
> multi-option billing dialog where a blind Enter could have declined the switch it was meant to
> complete. Rows also carry `display` and `family`, added after the label turned out not to name
> the model — the server labels `claude-fable-5-1[1m]` as bare "Fable", so a client matching on
> `name` told four live Fable 5 sessions they were already on Fable 5.1. `name` is now cosmetic by
> contract and the two matchable fields are derived from `value`. There is deliberately no
> `resolves`: what an alias lands on is decided inside Claude Code at switch time and maiTerm
> cannot observe it, so a guess matched exactly would be silently wrong where a stated granularity
> is merely coarse.
>
> **v0.3 changelog** (agreed with the maiLink agent): the surface is now **topic-threaded**.
> Per-tab `/chats` is superseded by **`/threads`** (a thread is `kind:"topic"` for a mesh
> conversation or `kind:"solo"` for a lone agent tab); `thread_id` is the canonical key and
> `tabId` becomes a participant identity. Transcript turns carry `author` + `thread_id`;
> the WS `attention` event and doorbell context carry `thread_id` + `asked_by`. The pending
> ask is the agent's **native** human prompt — `AskUserQuestion` (structured) or a permission
> prompt — carried in `PendingPrompt` with a `respondable` flag (permission answers ship now;
> `AskUserQuestion` ships read-only first, answer-from-phone as a fast-follow). This kills the
> old agent-authored "status note / NEEDS DECISION" channel (desktop side, already removed):
> one ask in, one card rendered, one `/respond` out. Full contract + TS types in **§12**;
> it supersedes the chat-centric parts of §2.1, §4, §5, and §8. **Open product call (Darryl):**
> iOS-first vs iOS+Android launch scope (unchanged from v0.2; protocol supports both).
>
> **v0.2 changelog** (agreed with the maiLink agent): app stack is **Capacitor +
> SvelteKit + shadcn-svelte** (cross-platform, not native SwiftUI); `/push-register` is
> **platform-tagged** (APNs+FCM); the WS `attention` event carries an optional inline
> `prompt`; prompts have an opaque `prompt_id` carried through `/respond` (stale-guard);
> `POST /message`'s `msg_id` is guaranteed identical to its later WS echo; a transcript
> pagination param is reserved.

## 0. What maiLink is (and is not)

**maiLink is a lightweight mobile *companion* for the agents running inside maiTerm** —
not a terminal. When a Claude/Codex/Gemini agent in a maiTerm tab needs a human (a
permission prompt, a question, or it just finished and is waiting), maiLink rings your
phone; you read enough context to decide, and reply. And — because certain tabs/workspaces
can be designated **maiLink-native** — you can also *proactively* open one as a chat and
drive it from your phone, unprompted.

| | maiLink (this doc) | Full mobile maiTerm (`mobile-packaging.md`) |
|---|---|---|
| Product | Chat/approvals companion | Real remote terminal |
| Terminal core | **None** | `alacritty_terminal` + xterm.js + `russh` |
| Talks to | A running desktop maiTerm over LAN | Remote SSH hosts directly |
| Stack | Capacitor + SvelteKit + shadcn-svelte | Tauri mobile, ~80% reuse |
| Effort | Small, well-scoped | Weeks |

These are **independent** products. Don't conflate them. maiLink does not embed a terminal;
it renders a *distilled chat transcript* of an agent and injects replies back into it.

### Locked-in decisions (from product owner, 2026-06-27)

1. **Wake mechanism: thin doorbell.** The cloud is used *only* as a content-free bell
   (APNs for iOS). All real data — prompts, context, replies — flows over the **LAN /
   WireGuard** link. Apple/our relay never see terminal content.
2. **Platform: cross-platform via Capacitor (iOS + Android).** The contract is
   transport- and platform-neutral; push is platform-tagged (APNs + FCM). **Launch scope
   (iOS-first vs both-at-once) is an open product call for Darryl** — it does not affect
   the protocol.
3. **Interaction model: a chat app.** Bidirectional. Inbound = the agent needs/notifies
   you. Outbound = you can activate maiLink-native tabs and send proactive commands.
4. **Exposure: opt-in + per-device QR pairing.** The LAN listener is **off** until enabled
   in Preferences. Pairing is a QR scan that hands the phone host+port+cert-fingerprint+a
   one-time code. Each phone is a revocable device. **The existing localhost-only IDE/MCP
   server (`claude_code/server.rs`, bound to `127.0.0.1`) is untouched** — maiLink is a
   *separate*, explicitly-gated LAN surface.

---

## 1. Architecture

```
                       ┌──────────────────────── desktop maiTerm ────────────────────────┐
                       │                                                                   │
  Claude/Codex hooks ──┼─► agent_sessions (session→tab)   tab_pty_map / pty_registry      │
   (already exists)    │        │  state machine                  │                        │
                       │        ▼  (active/idle/permission)        ▼  write_pty()           │
                       │   agentStateStore  ──── attention ───►  bracketed-paste inject    │
                       │        │                events            ▲                        │
                       │        ▼                                  │                        │
                       │   ┌─────────────── NEW: mailink module ───┴──────────────┐        │
                       │   │  • maiLink-native registry (designated tabs/ws)       │        │
                       │   │  • gated axum listener on LAN iface (TLS, self-signed) │        │
                       │   │  • per-device pairing + bearer tokens                  │        │
                       │   │  • WS live chat channel + REST actions                 │        │
                       │   │  • doorbell trigger → relay when no live WS            │        │
                       │   └───────────────┬───────────────────────┬───────────────┘        │
                       └───────────────────┼───────────────────────┼────────────────────────┘
                                           │ LAN / WireGuard (TLS)  │ content-free wake
                                           │ (all real data)        ▼
                                           │                 ┌─────────────┐   ┌──────────┐
                                           │                 │ push relay  │──►│   APNs   │
                                           │                 │ (CF Worker, │   └────┬─────┘
                                           │                 │  holds .p8) │        │
                                           ▼                 └─────────────┘        ▼
                                   ┌──────────────────────────────────────────────────────┐
                                   │  maiLink iOS app  — chat list / thread / composer      │
                                   │  wakes on push ► opens WS over LAN ► pulls real data    │
                                   └──────────────────────────────────────────────────────┘
```

**Three new things on the desktop side. Everything else already exists.**

1. **maiLink-native designation** — a flag on tabs/workspaces marking them as "exposed to
   maiLink as a chat."
2. **Gated LAN bridge** — a new `src-tauri/src/mailink/` module: its own TLS axum listener
   (separate from the localhost IDE/MCP server), per-device pairing + tokens, a WS live
   channel, and REST actions. Lists maiLink-native chats, streams their state, accepts
   messages/commands, serves distilled context.
3. **APNs doorbell** — when an attention event fires for a maiLink-native tab and no device
   currently holds a live foreground WS, the desktop POSTs a content-free wake to the push
   relay, which signs and forwards to APNs.

### What we reuse verbatim (already built — see `claude_code/CLAUDE.md`)

| Need | Existing mechanism | Location |
|---|---|---|
| "Agent needs a human" signal | hook state machine: `permission` / `idle`(done) / `active` | `src/lib/stores/agentState.svelte.ts`; `agent-hook-*` Tauri events |
| session → tab → pty resolution | `agent_sessions` → `tab_pty_map` → `pty_registry` | `src-tauri/src/state/app_state.rs` |
| Inject a reply/command | `write_pty(state, pty_id, &bytes)` + bracketed-paste submit | `pty/manager.rs:551`; `src/lib/utils/agentPrompt.ts:36` |
| Don't inject while a human prompt is pending | `deliverable()` / `isAwaitingHumanInput()` gate, FIFO mailbox | `src/lib/stores/agentDelivery.ts`; `src/lib/agents/adapter.ts` |
| Distilled context for the phone | `get_terminal_recent_text(pty_id, n)` (plain text) | `src-tauri/src/commands/terminal.rs:524` |
| HTTP/WS/SSE server patterns, auth, conn affinity | axum server | `src-tauri/src/claude_code/server.rs` |
| A deployed Cloudflare Worker (precedent for the relay) | update + stats worker | `update-worker/` (`updates.maiterm.dev`) |

The reply path is the **same rails the agent-to-agent bridge already uses** — maiLink is
"just another peer" that happens to be a phone instead of a forked Claude.

---

## 2. Data model additions

### 2.1 maiLink-native designation

Add an additive, `serde(default)` flag to `Tab` (`state/workspace.rs:215`) and `Workspace`
(`:406`):

```rust
// Tab
#[serde(default)]
pub mailink_native: bool,      // this tab appears as a chat in maiLink

// Workspace
#[serde(default)]
pub mailink_native: bool,      // all agent tabs in this workspace are maiLink chats
```

**Effective availability** is chosen by the `mailink_expose_all` preference
(`Preferences`, `serde(default = "default_true")` — *on* by default):

* **expose-all (default):** every *agent* tab is available, minus per-tab opt-outs.
  Availability = `tab.runtime.is_some() && !tab.mailink_excluded`. "Is an agent tab" keys off
  the **persisted** `Tab.runtime` (set once at initSession, never cleared) rather than a live
  `agent_sessions` entry — so a tab whose agent has *stopped* (network drop, quit) stays
  available and can be auto-resumed from the phone.
* **designate-only:** availability = `tab.mailink_native || workspace.mailink_native`. This is
  the opt-in escape hatch and honors plain shells the user hand-picks.

Both branches are intersected with `TabType::Terminal`. The single choke point is
`designated_tabs()` in `mailink/mod.rs`. Mirrors the `Workspace.bridge_all` mesh pattern (see
mesh-workspace.md): designation is *persisted*, the live roster is *derived*.

Flags (`Tab`): `mailink_native` (opt-in, designate-only mode) and `mailink_excluded`
(opt-out, expose-all mode). `Workspace.mailink_native` is the workspace-wide opt-in.

> **Serde round-trip pitfall** (project-wide): `skip_serializing_if`/`default` means loaded
> JS objects get `undefined`, not `false`. Normalize with `?? false` on the TS side; never
> `JSON.stringify`-compare.

Commands (follow the New-Tauri-Command checklist in root `CLAUDE.md`):
`set_tab_mailink_native(tab_id, on)`, `set_tab_mailink_excluded(tab_id, on)`,
`set_workspace_mailink_native(ws_id, on)`; `mailink_expose_all` rides the bulk `set_preferences`.

UI: a tab right-click toggle — "Make (un)available in maiLink" (targets `mailink_excluded` in
expose-all mode, `mailink_native` in designate-only mode) — plus a Preferences "maiLink" section
(enable bridge, "Make all tabs available in maiLink" toggle, paired devices).

### 2.2 Preferences additions (`Preferences`, `state/workspace.rs:793`)

```rust
#[serde(default)] pub mailink_enabled: bool,                 // master on/off for the LAN bridge
#[serde(default)] pub mailink_port: Option<u16>,             // None → pick + persist a free port
#[serde(default)] pub mailink_bind: MailinkBind,             // Lan (0.0.0.0) | specific iface
#[serde(default)] pub mailink_devices: Vec<MailinkDevice>,   // paired devices (see below)
```

### 2.3 Paired device record (persisted, in state — not preferences if it carries secrets)

```rust
pub struct MailinkDevice {
    pub id: String,            // uuid
    pub name: String,          // "Darryl's iPhone" (user-editable)
    pub token_hash: String,    // argon2/sha256 of the bearer token (never store raw)
    pub push_token: Option<String>,   // device's push token (APNs or FCM), set after pairing
    pub push_platform: PushPlatform,  // Apns | Fcm — which sender the relay uses
    pub push_env: PushEnv,            // Sandbox | Production (APNs); maps to project for FCM
    pub created_at: i64,
    pub last_seen_at: i64,   // any authenticated request or WS connect, throttled to one write
                             //   per 5 min. It DOES mean liveness — it previously advanced only
                             //   on pairing and push-token registration, so a phone talking to
                             //   the desktop all day could read as two days gone.
}
```

Revocation = remove the record; its bearer token stops validating immediately.

**Rejections are logged.** Every response ≥400 emits a WARN with method, path, status and who the
caller claimed to be — the device name if the token matches a paired device, otherwise a short hash
prefix and how many devices exist. Nothing is logged for successful requests (the phone polls every
couple of seconds). This exists because a rotated or expired token was otherwise *completely*
silent: the phone showed an empty inbox, the desktop log showed nothing at all, and that combination
is indistinguishable from "the desktop genuinely has no chats" — which is exactly the wrong
conclusion. A 401 in the log names the fix (re-pair) directly.

---

## 3. Pairing & auth

### 3.1 TLS on the LAN (required — and ATS-compatible)

The LAN listener serves **HTTPS with a self-signed cert** generated on first enable
(`rcgen` crate). This is non-negotiable: without TLS the WireGuard'd link is still cleartext
to anything on the same LAN, and mobile OSes won't trust an untrusted chain by default. We
satisfy this via **cert pinning**: the QR carries the cert's SHA-256 fingerprint; the app
pins it. Self-signed + pinned = encrypted *and* MITM-resistant, no CA needed.

> **Capacitor note (maiLink agent owns this):** in a Capacitor WebView, JS `fetch`/
> `WebSocket` cannot override trust for a self-signed cert (WKWebView / Android WebView
> reject it; `NSAllowsLocalNetworking` relaxes ATS but still won't trust an untrusted
> chain). So maiLink ships a **thin native transport plugin** owning REST + WS with
> pinned-fingerprint trust evaluation — iOS `URLSession` `didReceive` challenge +
> `URLSessionWebSocketTask`; Android OkHttp custom `TrustManager` + WebSocket. This is the
> app's responsibility and changes none of the desktop handlers; pinning is solved
> native-side on both platforms, not in JS.

**Fingerprint format (FROZEN — agreed with the maiLink agent, v0.2):** the QR `fp` field is

```
fp = "sha256/" + base64( SHA256( DER_of_leaf_cert ) )
```

- **Hashed input:** the server's **leaf certificate, full DER** — the whole cert, NOT the
  SPKI/public-key. These are the exact bytes `cert.der()` returns from `rcgen` on the desktop,
  `SecCertificateCopyData` on iOS, `X509Certificate.getEncoded()` on Android — so all three
  hash identical bytes. (Full-cert avoids the iOS SPKI ASN.1-header reconstruction footgun.)
- **Hash:** SHA-256. **Encoding:** standard Base64 (RFC 4648, `+`/`/`, `=`-padded) — **not**
  base64url. **Prefix:** literal `sha256/`.
- **Note:** here `sha256/` denotes a **full-cert (leaf DER) pin**, NOT OkHttp's SPKI
  `CertificatePinner` convention — the app uses a custom trust evaluator, so the prefix is
  just our shared label. Don't assume SPKI on either side.
- **Reproduction (both sides must print the same value):**
  `openssl x509 -in cert.pem -outform DER | openssl dgst -sha256 -binary | base64`

**Verification is fingerprint-only — hostname/SAN is intentionally bypassed.** With a pinned
self-signed cert, SAN/host matching is redundant and would only cause spurious failures (LAN
IP absent from SAN, or IP churn). Consequences, all intended: (1) the cert needs **no IP in
its SAN**; (2) the **same cert validates at any IP**, so a DHCP address change (or
mDNS-rediscovery) reconnects **without re-pairing**; (3) the pin changes **only** when the
desktop regenerates the cert — then the QR carries the new `fp` and the device re-pairs. One
native trust delegate covers **both** REST and WSS (iOS `URLSessionDelegate.didReceive`
serverTrust challenge; Android custom `X509TrustManager`) — REST and WSS share the anchor.

### 3.2 QR pairing handshake

```
QR payload (JSON, displayed by Prefs ▸ maiLink ▸ "Pair new device")
{ "v": 1,
  "host": "192.168.1.42",          // or the WireGuard peer IP
  "port": 9787,
  "fp": "sha256/BASE64CERTFP",     // cert fingerprint to pin
  "code": "RXT7-9K2Q",             // one-time pairing code, TTL ~120s, single use
  "name": "Darryl's MacBook" }
```

1. App scans QR, dials `https://host:port` pinning `fp`.
2. `POST /mailink/v1/pair  { code, device_name, app_info }`
   → desktop validates `code` (unexpired, unused) → mints a long-lived **bearer token**,
   stores `MailinkDevice{ token_hash, name }`, returns `{ device_id, token, server_name }`.
   The raw token is shown to the phone **once**; desktop keeps only its hash.
3. App stores `token` in the iOS Keychain. All later calls send
   `Authorization: Bearer <token>` over the pinned-TLS channel.
4. App mints its **relay capability**: `POST {relay}/push-capability { push_token, platform }`
   → `{ cap }` (see §6). This is a one-time call to the *shared relay* (not the desktop), and
   `cap` is what authorizes the desktop to ring this device on the multi-tenant relay.
5. App registers for push with the desktop:
   `POST /mailink/v1/push-register { token, platform, env, cap }` where `platform` is `"apns"`
   or `"fcm"`. The desktop stores `cap` on the device record and presents it on every wake.

The pairing code is the only out-of-band secret and it's short-lived + single-use; the
bearer token never transits a QR or a screen after step 2.

> **WireGuard note:** maiLink imposes nothing on the VPN. When off-LAN, the user brings up
> their WireGuard tunnel (any client) and the QR/host simply carries the WG peer IP instead
> of the LAN IP. From maiLink's perspective it's the same TLS endpoint. We *document* a
> recommended WG setup; we don't ship a VPN.

---

## 4. The wire contract (LAN API)

Base: `https://{host}:{port}/mailink/v1`. Auth: `Authorization: Bearer <token>` on
everything except `/pair`. JSON bodies. All times are unix ms.

### 4.1 REST (stateless actions)

| Method + path | Purpose | Body / returns |
|---|---|---|
| `POST /pair` | Redeem QR code → token | `{code,device_name}` → `{device_id,token,server_name}` |
| `POST /push-register` | Store push token + relay capability for doorbell | `{token,platform,env,cap}` → `{ok}` (`platform`: `"apns"`\|`"fcm"`; `cap` from §6 `/push-capability`) |
| `GET  /chats` | List maiLink-native chats + state | → `Chat[]` (see §4.3) |
| `GET  /models` | What this machine can switch a Claude tab to — so the picker stops hardcoding a list that goes stale on every Claude release | → `ModelOption[]` |
| `GET  /tasks` | The maiTerm task board (§4.3 `TaskBoard`): every workspace with a designated tab, its workstreams and rows. Pure in-memory read, safe to poll; fetch once on roster load and let the WS `tasks` event keep per-tab rows current | → `TaskBoard` |
| `GET  /assets` | Every file an agent sent, newest first, across all chats — the Files view | → `FileAsset[]` (max 200) |
| `GET  /assets/{assetId}` | The bytes | → the file. `Accept-Ranges: bytes`; honours `Range` with `206` + `Content-Range`, `416` for a start past the end. `Content-Type` from the name, `Content-Disposition: attachment` with both `filename=` and `filename*=`. `404` when unknown OR evicted — but the descriptor's `available` already said so, so never discover it here |
| `GET  /chats/{tabId}?before={msg_id}&limit=N` | One chat + transcript (paging params reserved) | → `ChatDetail` |
| `GET  /chats/{tabId}/context?lines=N` | Distilled plain-text context | → `{text, truncated}` |
| `POST /chats/{tabId}/message` | Send a message / proactive command (auto-wakes an unregistered tab first — §5) | `{text, submit?:true}` → `{status:"delivered", msg_id, woke:null\|"init"\|"resume"}` \| `{status:"unreachable", reason, detail}` |
| `POST /chats/{tabId}/respond` | Answer a pending permission/question | `{choice, prompt_id}` (see §5) → `{ok}` \| `{ok:false, reason:"stale"}` |
| `POST /chats/{tabId}/activate` | Activate/focus/resume a designated tab | `{}` → `{state}` |
| `POST /chats/{tabId}/interrupt` | Send Esc (stop the agent); settles chat state to idle, clears the restored prompt | `{}` → `{ok, settled, composerCleared}` (may hold up to 3 s — §5) |
| `POST /chats/{tabId}/shells/{shellId}/stop` | Terminate one background shell (SIGTERM→SIGKILL on its own pid) | `{}` → `{ok:true, stopped:bool}`; IDEMPOTENT — an already-dead or unknown shell is `ok:true, stopped:false`, never 404 |
| `POST /chats/{tabId}/new` | Start a NEW conversation from this one (light clone: SSH host + cwd, fresh agent session) | `{}` → `{ok:true, tabId}`; `{ok:false, reason:"timeout"}` if the tab didn't appear in time |
| `POST /chats/{tabId}/rename` | Set the tab title | `{title}` → `{ok, title}` (normalized) |
| `POST /chats/{tabId}/resume-workspace` | Wake the suspended workspace that owns this tab | `{}` → `{ok, resumed, workspaceId?}` |
| `POST /chats/{tabId}/wake` | Per-tab Initialize — re-register or restart this tab's agent | `{}` → `{ok:true, woke:"init"\|"resume"}` \| `{ok:true, woke:null, reason, detail?}` |
| `POST /chats/{tabId}/queue/cancel` | Pull back the ONE message waiting in the input queue (§5) | `{}` → `{ok:true, cancelled:true, text, composerCleared}` \| `{ok:true, cancelled:false, reason}` |
| `POST /chats/{tabId}/mesh-init` | Initialize-all for the mesh workspace that owns this tab | `{}` → `{ok, initiated, workspaceId?, reason?}` |
| `GET  /chats/archived` | Archived (recoverable) tabs across all workspaces | → `ArchivedChat[]` |
| `POST /chats/{tabId}/archive` | Archive a live tab (RECOVERABLE) | `{}` → `{ok}` |
| `POST /chats/{tabId}/close` | End a live tab PERMANENTLY (destructive) | `{}` → `{ok}` |
| `POST /chats/{tabId}/restore` | Un-archive a tab back into its workspace | `{}` → `{ok, workspaceId?}` |
| `GET  /heartbeat` | Liveness + server clock | → `{ok, now, server_name}` |

### 4.2 WebSocket (live chat channel) — `GET /mailink/v1/ws` (upgrade)

Bidirectional, opened while the app is foreground. Server→client events:

```jsonc
{ "type": "chat_state", "tabId": "...", "state": "active|idle|permission",
  "runtime": "claude", "tool": "Bash", "detail": "rm -rf ./dist",
  "registered": true, "prompt": null, "ts": 0 }
                                                     // FIELD-PRESENCE CONTRACT, and the general form of the merge rule
                                                     // spelled out for `registered` below. A `chat_state` frame is MERGED
                                                     // over a row built by GET /chats — two producers, one row — so an
                                                     // omitted field is indistinguishable from a field the frame is
                                                     // claiming is empty. Therefore: **every field above is always present
                                                     // on every frame, carrying an explicit `null` where there is nothing.**
                                                     // `meta` is the single exception: it is omitted when the desktop
                                                     // cannot resolve it (non-Claude tab, transcript momentarily
                                                     // unreadable), which means "unknown", so merge it only when present
                                                     // and never let a transient miss blank a live gauge.
                                                     //
                                                     // Clients: apply present fields, ignore absent ones. That single rule
                                                     // covers `registered` and every field added later, and is what an
                                                     // older desktop (which omits newer fields entirely) needs too.
                                                     // `ts` is NOT when this event happened — it is the tab's real
                                                     // `lastActivityTs`, and both fields carry the same value. So it does
                                                     // NOT advance on every frame: a prompt opening or closing moves no
                                                     // transcript turn, so a frame announcing one repeats the previous
                                                     // `ts`. Never use it to decide whether a frame is newer than what you
                                                     // hold — `ev.ts <= row.lastActivityTs` discards exactly the frames
                                                     // that report a prompt change. Frames arrive in order on one socket;
                                                     // apply them.
                                                     // `prompt` mirrors the field on Chat. It is present because this frame
                                                     // FIRES on prompt changes — the emit key is state+prompt — so a frame
                                                     // that carried only `state` announced that something moved while
                                                     // withholding what. Answering an AskUserQuestion is exactly that case
                                                     // (prompt question → null, `state` stays "active"): it raises no
                                                     // `attention` event, since those fire only INTO attention, and no
                                                     // `chats_changed`, since the roster diff does not watch prompt. Bind
                                                     // the "needs you" pin to this rather than to the value the last GET
                                                     // left behind, or an answered ask stays pinned.
                                                     // `registered` mirrors the field on Chat/ThreadDetail. It is the ONLY
                                                     // live signal an already-open thread gets about registration, so bind
                                                     // the "running but not registered" banner to it rather than to the
                                                     // value fetched when the thread opened. A frame is emitted whenever it
                                                     // flips, even though `state` does not move: a live agent reads "active"
                                                     // both before and after it registers, so the init that clears the
                                                     // banner changes nothing else on the wire.
                                                     // MERGE RULE — apply `registered` only when the field is PRESENT
                                                     // (`if (ev.registered !== undefined)`). A desktop older than this
                                                     // field omits it from every frame, and frames fire constantly during
                                                     // a turn; merging absent-as-true would clear a `registered:false`
                                                     // the GET had just established and hide the banner on exactly the
                                                     // desktops that still need it. Absence means "this server can't say",
                                                     // never "registered". Read in ISOLATION (a lone frame, no prior
                                                     // state) an absent field still defaults to `true` — it must never
                                                     // manufacture a re-initialize prompt.
{ "type": "message", "tabId": "...", "role": "agent|user|system",
  "text": "...", "msg_id": "...", "ts": 0 }          // a new transcript turn
                                                     // `kind: "asset"` turns also carry `assets: FileAsset[]` here — a file
                                                     // an agent just sent, landing live in an open chat. Positional, not a
                                                     // full-replace strip: it is a timeline entry and has a place in the
                                                     // order. A client that doesn't know the kind renders `text`
                                                     // ("Sent 2 files: report.pdf, clip.mov") rather than breaking.
                                                     // for a user echo, msg_id === the id POST /message returned
{ "type": "attention", "tabId": "...", "kind": "permission|idle_done|question",
  "summary": "Needs permission: Run rm -rf ./dist",
  "prompt": { "prompt_id": "p_7f3a", "kind": "permission", "text": "Run: rm -rf ./dist",
              "options": ["Yes","Yes, don't ask again","No"] }, "ts": 0 }
                                                     // `prompt` mirrors pendingPrompt; present for permission/question,
                                                     // omitted for idle_done. Lets the app render decision buttons on the
                                                     // live path with no follow-up GET. GET /chats/{tabId} stays source of truth.
{ "type": "chats_changed" }                           // roster/designation, a tab title, a workspace's suspended flag, its mesh
                                                     // flag, OR a tab's `registered` flag changed; re-GET /chats. This is a
                                                     // ROSTER signal only — it does not refresh an open thread, which is why
                                                     // `registered` also rides on `chat_state`.
{ "type": "tasks", "tabId": "...", "tasks": [/* MaitermTask[] */], "ts": 0 }
                                                     // the tab's maiTerm task rows changed — REPLACE the whole array (a tab's
                                                     // rows are few; no per-task diffing). ANY runtime, and a tab with no live
                                                     // session too: the task store is maiTerm's, not the agent's. Emitted for
                                                     // every designated tab that owns rows on WS connect (baseline — so the
                                                     // roster-wide per-tab counts need nothing beyond this plus one GET /tasks),
                                                     // then only on change; [] ONCE when a tab's rows empty; never for a tab
                                                     // that never had any. "Change" includes what a row only REFERENCES — a
                                                     // workstream or tab rename, or a blocker finishing on another tab — because
                                                     // the change key hashes what the rows RENDER AS, not the rows' own fields.
                                                     // Nothing deletes rows on completion: `done` is a lane.
{ "type": "shells", "tabId": "...", "shells": [/* AgentShell[] */], "ts": 0 }
                                                     // the tab's background-shell roster changed — REPLACE the whole
                                                     // array. Same baseline-on-connect discipline as `tasks`; [] clears.
                                                     // Fires on a shell EXITING too, which appends nothing to the
                                                     // transcript. Claude + local tabs only.
{ "type": "subagents", "tabId": "...", "subagents": [/* Subagent[] */], "ts": 0 }
                                                     // the tab's delegation roster changed — REPLACE the whole array.
                                                     // Same baseline/[] discipline. The change key folds in `lastLine`,
                                                     // so a RUNNING delegation re-emits as its progress moves: a
                                                     // status-only key would send one frame at launch and one at
                                                     // completion, leaving a five-minute-old sentence on screen in
                                                     // between — which is the gap this event exists to close.
```

Client→server frames are optional conveniences mirroring the REST actions (`message`,
`respond`, `activate`, `interrupt`) so the foreground app can avoid REST round-trips; both
paths converge on the same backend handlers.

Presence: while ≥1 device holds a live WS for a tab, that tab is "covered" and the doorbell
is **suppressed** (no redundant push). On WS close, coverage drops and future attention
events doorbell again.

**A registration edge is not a finished turn.** Both announcers — the push doorbell and the WS
`attention` frame — ring only when a tab crosses INTO attention *and* already had a tracked
session (`registered`) when it was last observed. A first sighting baselines silently, and so does
a tab's session row first appearing. This matters at desktop startup: session tracking is
in-memory, so every tab launches `dormant` + `registered:false`, and moments later each agent's
SessionStart registers it as idle — an attention transition on every resumed tab at once, while
nothing can be covered because the desktop was down. Clients need no logic for this; it is stated
so nobody re-derives the naive edge.

**Clients MUST answer WebSocket Pings.** The desktop pings every 20s and closes the socket after
one unanswered ping. This is load-bearing rather than hygiene: coverage suppresses the doorbell, so
a socket that is dead-but-ESTABLISHED — an iOS app suspended in the background, a phone that walked
off the LAN — makes the desktop believe a phone is watching when nothing is. Worse than delaying
the push, it *loses* it: the doorbell records each attention transition as it observes it and skips
ringing while covered, so anything that happened during phantom coverage is never pushed even after
the socket finally errors out minutes later. Exactly the away-from-the-desk case maiLink exists for.
The first ping goes out **at connect**, not after an interval: a client is provably awake the
instant it completes a handshake, which is the only moment a pong is guaranteed to be answerable.
Ping-after-an-interval would routinely land in an app iOS had already suspended — open maiLink,
glance, pocket the phone — so "this client has never ponged" would be a race with suspension rather
than a fact about the client.

An unanswered ping always closes the socket; there is no "maybe this client can't pong" escape
hatch. A never-ponged connection is logged at WARN and closed anyway, because a reconnect loop is
loud and self-announcing while phantom coverage is silent and eats notifications. Verified against
both transports: iOS `URLSessionWebSocketTask` and Android OkHttp `RealWebSocket` answer at
framework level, with control frames never reaching app code.

### 4.3 Shapes

```typescript
interface Chat {
  tabId: string;
  title: string;            // tab name
  workspace: string;        // workspace name (grouping)
  workspaceId: string;      // owning workspace id (stable; resume flow resolves tab→workspace)
  workspaceSuspended: boolean; // owning workspace is suspended → show "Resume workspace", not Initialize
  mesh: boolean;            // owning workspace is a Mesh Workspace → badge the group, offer Initialize-all
  runtime: 'claude' | 'codex' | 'gemini';
  state: 'active' | 'idle' | 'permission' | 'dormant';
                            // dormant = no live agent. A tab with a live PTY that is still producing
                            // turns but whose session registration was lost (e.g. a mesh/SSH resume
                            // where the hook/init handshake missed) reports 'active' via a
                            // self-correcting liveness fallback — never a stuck 'dormant' over live output.
                            //
                            // **`idle` does NOT mean "a turn finished".** It also means "a process is
                            // alive and sitting at an empty prompt", which is the resting state of
                            // every tab after a desktop restart — an agent registers as idle the
                            // moment it comes up, before anyone has typed anything. Do not build
                            // "has a result", "needs you", or "worth surfacing" on bare `idle`; use
                            // `unread` (which the desktop derives from a turn actually ending) or
                            // `prompt`. Three desktop-side predicates got this wrong in a row — the
                            // push doorbell, the WS `attention` frame, and `unread` itself — each
                            // announcing a finished turn for every tab that had merely come back up.

  registered: boolean;      // false ⇒ this tab has NO tracked agent session, so `state` was
                            //   inferred from a liveness fallback (live PTY + a recent transcript
                            //   turn ⇒ "active", else "dormant") rather than observed. A live
                            //   agent that never registered is neither dormant nor working, and
                            //   `state` has no word for it — so don't read too much into the word;
                            //   offer a re-initialize action instead. Always true when a session
                            //   is tracked.
  unread: boolean;          // this chat holds something a human has not read: an open prompt, or
                            //   a turn that ENDED here. Deliberately NOT "state is idle" — see the
                            //   note under `state` below. Still not per-device: nothing clears it
                            //   until the agent works again.
  lastActivityTs: number;
  preview: string;          // last line(s) of distilled context
  tool: string | null;      // the tool the agent is running RIGHT NOW, and its primary argument
  detail: string | null;    //   ("Bash" / "npm test", "Edit" / "src/lib.rs"). From the PreToolUse
                            //   hook; both cleared on Stop, so an idle agent reports null for
                            //   both — never the last tool of the last turn. `preview` above is
                            //   maiTerm's own phrasing of the same facts ("Working… (Bash)");
                            //   these are the raw pair, so a client renders its own wording
                            //   rather than parsing ours. Also on ChatDetail and every
                            //   `chat_state` frame — the live half is where the value is, and it
                            //   needed its own diff (§4.2): these move within a turn while
                            //   `state` sits at `active`.
                            //
                            //   SHIPPED IN 0.8. Documented on the `chat_state` example since
                            //   v0.4 and never actually sent by any desktop — a client reading
                            //   them before 0.8 was reading a field with no producer, which is
                            //   why a phone showed "working…" while an agent ran tools for
                            //   minutes. The doc was right; the code was the half that was wrong.
  windowLabel: string;      // the window that owns this tab. Overlord is per WINDOW and every
                            // action is addressed POST /overlord/{windowLabel}/…, so this is
                            // what lets an ordinary thread — no escalation naming it, no tasks —
                            // still offer `recover` and `fireRule`. Also on ChatDetail.
}
interface ChatDetail extends Chat {
  transcript: Message[];    // distilled turns, newest last
  tasks: MaitermTask[];     // this tab's maiTerm tasks (docs/tasks.md) — the rows whose `tabId`
                            // is this chat, in BOARD ORDER. ALWAYS present, `[]` when the tab owns
                            // none: a phone that sees no key is talking to a pre-v0.5 desktop, not
                            // an idle tab. Live updates ride the WS `tasks` event (full replace).
                            // v0.5 REPLACED the Claude session board (`AgentTask`) here — see the
                            // changelog for why, and `MaitermTask` for the rules.
  queued?: { text: string; queuedAt: number }[];
                            // messages typed while the agent was BUSY and not yet consumed, oldest
                            //   first. Render these as genuinely "queued" (the agent is busy),
                            //   not as an in-flight spinner — and treat presence here as the
                            //   precondition for offering to pull one back: an already-consumed
                            //   message cannot be recalled. `text` matches what the turn will echo
                            //   once consumed. Claude tabs only; absent when the queue is empty.
                            //   GET only — there is no `queued` WS event; re-GET on a state change.
                            //   Reconstructed by replaying `queue-operation` lines, and only
                            //   `enqueue`/`popAll` carry text: `remove` and `dequeue` are bare
                            //   records, so which entry drained comes from the `queued_command`
                            //   attachment that follows a `remove`, and from FIFO order otherwise.
                            //   Do NOT read `dequeue` as an arrow-up recall — 1042 of 1229 in a
                            //   real corpus are followed by an ordinary user turn. Nothing in a
                            //   transcript marks a genuine recall.
  goal?: AgentGoal;         // the `/goal` condition the agent is being held to. Claude tabs only;
                            //   absent when there is no goal. GET only — re-GET on a state change.
                            //   Scoped to the tab's CURRENT session: the Stop hook is
                            //   session-scoped, so a goal read off any other session would be one
                            //   nothing is actually enforcing. Costs a transcript tail read, so it
                            //   is detail-only and never on the chat list.
  shells?: AgentShell[];    // background shells (`Bash run_in_background` — the TUI's /bashes
                            // list), present only when non-empty. Claude + LOCAL tabs only: an
                            // SSH tab's shells are the REMOTE host's processes, so their liveness
                            // can't be confirmed and Stop couldn't signal them — those tabs report
                            // nothing rather than a roster that can't be stood behind.
  subagents?: Subagent[];   // delegations (the `Agent` tool), present only when non-empty. Claude
                            // tabs, LOCAL AND SSH — unlike `shells`: a subagent is not a process
                            // whose liveness needs the local process table, its whole lifecycle is
                            // written to the parent transcript, and the mirror shadows that. An
                            // SSH tab loses only `lastLine`. There is no Stop verb to be wrong about.
  pendingPrompt?: {         // present iff state==='permission' or a question is open
    prompt_id: string;      // opaque, minted when the agent opens this prompt; echoed in /respond
    kind: 'permission' | 'question';
    text: string;
    options?: string[];     // e.g. ["Yes","Yes, don't ask again","No"]; absent ⇒ free-text only
    asked_at?: number;      // question only: unix ms the ask opened — DISPLAY-ONLY ("asked 2m ago")
    expires_at?: number;    // question only, AUTHORITATIVE: unix ms the ask will auto-resolve.
                            // Sent only when the CC build+settings actually expire it (§11);
                            // absent ⇒ no countdown, answerable until the prompt clears.
  };
}
// msg_id identity guarantee: the id POST /message returns IS the id later emitted on the
// `message{role:'user'}` WS echo for that turn (mints at accept-time, reused for both) —
// lets the app reconcile an optimistic local bubble against the echo.
interface Message { msg_id: string; role: 'agent'|'user'|'system'; text: string; ts: number; }

// A file an agent sent to the phone (`sendFilesToPhone`). Appears twice: as a `kind:"asset"` turn
// in its chat's transcript, and in GET /assets across all chats.
interface ModelOption {
  value: string;            // exactly what goes after `/model`
  name: string;             // COSMETIC ONLY — never match on it, never parse it. On an account row
                            // it is the server's marketing label, which does NOT name the model:
                            // `claude-fable-5-1[1m]` is labelled just "Fable". Matching on it lit
                            // "already on this" for Fable 5 sessions against the Fable 5.1 row.
  display: string;          // what `meta.model` reads for a session on THIS value — the same
                            // renderer the chat rows use, so compare the two strings directly.
                            // An alias renders with NO version ("Opus"), because a version is not
                            // something an alias names; so an "Opus 4.6" session correctly does
                            // not equal the `opus` row.
  family: string;           // lowercase family this row switches into: opus|sonnet|haiku|fable.
                            // Stated so a client can offer a deliberate family-granular
                            // affordance without inferring one from punctuation in a label.
                            // Never empty: a value naming no family names no model, and is dropped.
  ambiguous: boolean;       // another row in THIS response shares this `display`, so matching it
                            // does not identify this row. Set on every [1m] pair — the window
                            // marker is not part of a model's identity and `display` strips it, so
                            // `opus` and `opus[1m]` both render "Opus" and a session stamping the
                            // bare alias equals both, though only one leaves its window alone.
                            // maiTerm CANNOT break that tie: a session's window is inferred per
                            // family (transcripts do not reliably carry the marker), not observed.
                            // So: treat a match on an ambiguous row as "on this model", never as
                            // "on this row", and never promise an outcome that only telling the
                            // two apart could confirm.
  note: string;
  source: 'account' | 'builtin';
                            // NOT decoration — the two are not equally trustworthy.
                            // 'account' came from this account's server-pushed model cache
                            //   (~/.claude.json additionalModelOptionsCache): the account was
                            //   really told about it, with the server's own label and value.
                            //   This is how Fable 5.1 appeared with no code change anywhere.
                            // 'builtin' is maiTerm's curated tier table — expected on every
                            //   install, verified on none. There is an entitlement hook
                            //   (modelAccessCache) but it is empty in the wild, so nothing here
                            //   can confirm the account actually has it. Offer these, but a
                            //   switch that gets refused at the TUI is a 'builtin' row's failure
                            //   mode, not a bug.
  // Account values are PINNED ids (`claude-fable-5-1[1m]`) because that is what the cache holds;
  // builtin values are aliases (`opus[1m]`) so a point release inside a tier needs no edit.
  // maiTerm does not rewrite one into the other — inventing an alias it cannot verify would fail
  // at the TUI in front of the human instead of here, where it can simply not be claimed.
  //
  // CLIENT CONTRACT for the three strings. They are server-pushed, maiTerm does not write them,
  // and they cross into a separate codebase that renders them. maiTerm strips control characters
  // and trims, and DROPS any entry whose `value` needed altering — a repaired id is one we no
  // longer know switches to the model the row names, and absent is visible where subtly-wrong is
  // not. `name` and `note` are repaired rather than dropped, since losing a whole model over a
  // stray character in its description is the worse trade. Beyond that `value` is byte-for-byte
  // what the cache held: clients MUST still treat it as untrusted input when typing it, and MUST
  // NOT render `name`/`note` as markup.
  // No `confirm` field, deliberately: whether `/model X` pops a confirmation is a fact about
  // Claude Code's TUI that maiTerm cannot observe, and asserting it would be the same hardcoded
  // guess one process further along.
  // CORRECTION (2026-09-01): an earlier draft of this section said such a confirmation "surfaces
  // through the normal pending-prompt path". It does NOT. `prompt` is only ever set from an
  // AskUserQuestion tool call or `state == "permission"`, both hook-driven; a TUI modal is
  // neither, so a tab parked at one most likely reads plain `idle` and the switch silently does
  // not happen. Verify the outcome instead — `meta.model` changing on the next reply — rather
  // than waiting for a prompt that will not arrive. Unverified on hardware; see the board item.
  //
  // No `resolves` field either, and this one is a REQUEST maiTerm declined rather than an
  // oversight. It would carry the concrete id an alias lands on (`opus` → `claude-opus-5`) so a
  // client could match exactly everywhere. maiTerm does not know it: alias resolution happens
  // inside Claude Code at switch time, nothing in maiTerm's state records it, and observation
  // cannot recover it — this machine ran claude-opus-4-8 and claude-opus-5 within two minutes of
  // each other, because a session's id reflects what its tab was set to, not what the alias
  // yields today. Sending a guess would be worse than sending nothing: a client matching it
  // EXACTLY and confidently would show no checkmark for a session genuinely on the alias's
  // target, and would be silently wrong. `display` + `family` give the same ergonomics at the
  // granularity that is actually knowable.
}

interface FileAsset {
  asset_id: string;         // uuid — unguessable on purpose; a leaked id is a leaked file
  name: string;             // the original filename, what the phone saves it as
  mime: string;             // from the extension only. Unknown ⇒ "application/octet-stream",
                            //   which routes to the share sheet — the honest answer, never a
                            //   wrong claim about what the file is
  bytes: number;            // the real cost of a tap, and the ONLY cost signal a video row gets
  ts: number;
  tabId: string;
  caption?: string;         // omitted when the agent gave none
  available: boolean;       // false ⇒ the bytes were evicted; render a tombstone with no tap.
                            //   ALWAYS present. Do not infer this from a failed fetch: a 404
                            //   cannot distinguish an eviction from a broken server from an
                            //   expired token, and a client forced to guess guesses wrong
}
// NO width, height, duration, or thumbnail endpoint, and none is coming. maiTerm has no image or
// video decoder and should not acquire one. The phone plays a downloaded file from its own
// container, where the media element is same-origin — so it reads duration/dimensions off
// `loadedmetadata` and draws its own poster frame, for free, and caches both against `asset_id`.
// A video row therefore renders from name + bytes + mime alone until first open, and improves
// itself after. Caps: 1 GB per file (refused above, with the size, by the tool), 10 GB store,
// oldest-first eviction.

// One background shell. Reconstructed from the transcript (identity + observed outcomes) and then
// SETTLED AGAINST THE OS PROCESS TABLE, because the transcript alone is badly stale: nothing is
// appended when a background process exits, so its "running" only ever means "was running when
// last polled". Measured on a real 37-shell session: the transcript claimed 31 running; exactly 1
// process existed. So `status:"running"` here means a live process was CONFIRMED — it is the only
// status that offers a Stop button, and it never lies about a dead process.
//
// The confirmation can't just count the agent's children: a live agent's direct children ALSO
// include its stdio MCP servers, which look exactly like long-running processes and would each be
// mistaken for a background shell. Matching is therefore keyed on the wrapper Claude Code actually
// spawns (`bash -c source …/shell-snapshots/… && eval '<command>'`), which no MCP server has.
interface AgentShell {
  id: string;               // Claude Code's shell id, e.g. "bkbod6zxj"
  command: string;          // the command line as the agent wrote it
  description?: string;     // the agent's own short label, when it gave one
  status: 'running' | 'completed' | 'failed' | 'killed';
  exitCode?: number;        // ABSENT on a terminal status means the outcome was never observed —
                            //   the shell ended without a final poll, so no code was recorded.
                            //   Never guessed. Render such rows as "ended", NOT as succeeded.
  startedAt: number;        // unix ms
  endedAt?: number;         // absent while running, and when the end went unobserved
  tail?: string;            // last captured output line — a progress hint, not the log
}

// A DELEGATION — Claude Code's `Agent` tool. Rendered like `shells`: a strip with elapsed time.
//
// Why it needed its own surface: Overlord asks for a review on nearly every change, so this is
// the most frequent minutes-long thing an agent does, and it reached the phone as ONE tool chip
// at launch and nothing after. A five-minute review was one line that never changed, so a thread
// deep in a code review said only "working…".
//
// **The lifecycle is not use→result.** The `Agent` tool returns in ~2s with an acknowledgement
// ("Async agent launched successfully. … agentId: …"), not an answer; the outcome arrives later
// as a separate `<task-notification>` turn. A client (or a desktop) that pairs tool_use with
// tool_result marks every delegation done two seconds after it started. The roster is keyed by
// the **agentId** from the ack — not the tool_use id, which the notification does not use.
interface Subagent {
  id: string;               // the agentId, e.g. "ad45d0719f4187070"
  description: string;      // the agent's own 3-5 word label ("Review the start-verb wiring").
                            //   Always present: "a subagent is running" without saying WHICH is
                            //   barely better than "working".
  agentType?: string;       // 'code-reviewer', 'Explore', 'general-purpose', … when declared
  status: 'running' | 'done' | 'failed';
                            // 'failed' covers every non-success outcome (cancelled, errored) —
                            //   the desktop never guesses which. A `done` that carries no
                            //   `lastLine` means it ended unobserved, the same convention
                            //   `AgentShell.exitCode` uses: render "ended", not "succeeded".
                            //
                            // **`done` IS NOT A ONE-WAY EDGE — do not fire a completion haptic,
                            //   badge or toast off the running→done transition.** Two ways an
                            //   entry legitimately goes back to `running`. A finished subagent can
                            //   be sent back to work (`SendMessage to: <agentId>`), and the CLI
                            //   emits the next notification only when that re-run stops. And when
                            //   nothing has been heard for a long time the desktop INFERS an end
                            //   (below); that inference is re-derived each poll, so it reverses as
                            //   soon as the agent proves otherwise. Render `done` as a state, not
                            //   as an event.
  startedAt: number;        // unix ms — when the CURRENT run started. RESET when a finished agent
                            //   is resumed, because elapsed exists to answer "how long has this
                            //   been going", and an hour-old launch time answers a question nobody
                            //   is asking about a run that began two minutes ago.
  endedAt?: number;         // absent while running, and absent on an INFERRED end (the desktop
                            //   knows it stopped but not when — same "never guessed" rule as
                            //   `AgentShell.exitCode`). Present ⇒ observed.
  lastLine?: string;        // the subagent's most recent words about its OWN progress — its last
                            //   assistant text while running, the opening of its result once done.
                            //   This is the "why" behind "working…": not "an agent is running"
                            //   but "reviewing the start-verb wiring — checking the busy map".
  lastLineTs?: number;      // unix ms the line was written. SENT AS A PAIR WITH `lastLine` or not
                            //   at all, and it is not decoration: this is the subagent's claim
                            //   about itself at a moment, it will sometimes be wrong or stale, and
                            //   a real one sat 9 minutes inside a single tool call. Show the age;
                            //   do not imply it is current. Same reason the Overlord card carries one.
}

// The `/goal <condition>` an agent is being held to. The goal installs a session-scoped Stop hook:
// the agent CANNOT end its turn until a judge decides the condition holds, and each attempt that
// falls short sends it back to work with a written explanation of what's missing. Away from the
// desk this is the best available answer to "is it actually going to finish, and what's left".
//
// Entirely transcript-derived (`goal_status` attachments in the session JSONL), which means it
// costs nothing extra on SSH tabs — the remote-JSONL mirror already carries it. No remote process
// is consulted, unlike `shells`.
interface AgentGoal {
  condition: string;        // as the operator typed it
  state: 'active' | 'met' | 'failed' | 'cleared';
                            // active  — being enforced right now: freshly set, OR evaluated and
                            //           sent back. A blocked verdict is NOT an ending; `reason`
                            //           then holds the judge's account of what's still outstanding.
                            // met     — the judge accepted it; the hook auto-cleared.
                            // failed  — the judge ruled the condition IMPOSSIBLE and dropped the
                            //           goal. Terminal, and NOT the same as met — the agent stopped
                            //           because it can't get there, which is worth surfacing loudly.
                            // cleared — the operator removed it by hand (`/goal` with no condition).
                            //
                            // The three terminal states are reported until the conversation moves
                            // past them (the session's next real turn), then the field drops. No
                            // clear record follows a met verdict, so without that window "the goal
                            // was met" and "there was never a goal" would arrive as the same
                            // absence and a completion could vanish unseen between two polls.
  attempts: number;         // evaluations of THIS goal so far; 0 before the agent's first turn ends.
                            //   Counted from the goal's set record, NOT taken from the record's own
                            //   `iterations` field: that counter lives in process memory and
                            //   restarts at 0 whenever a resume re-arms the hook, and it is written
                            //   only on terminal records — so it cannot be shown while a goal is
                            //   still running, which is exactly when it matters. (It is not
                            //   meaningless, though: it counts evaluations. It reads 1 throughout a
                            //   real corpus because those goals passed their FIRST check — including
                            //   one that took 65 minutes, which was one long turn, not many tries.)
  reason?: string;          // the judge's most recent verdict prose — what's done, what isn't,
                            //   quoted from the transcript. Absent until the first evaluation.
                            //   This is the payload of the feature; it is a paragraph, not a label.
  setAt?: number;           // unix ms the goal was set. Absent only if the set record is older than
                            //   the scanned transcript window — an evaluation still names the
                            //   condition, so the goal itself is never lost with it.
  lastCheckedAt?: number;   // unix ms of the most recent evaluation; absent until the first one.
  durationMs?: number;      // wall-clock and token cost, as Claude Code measured them. Emitted ONLY
  tokens?: number;          //   on a terminal verdict — a blocked evaluation carries neither — so
                            //   both are absent for the entire life of a running goal. Optional by
                            //   OUTCOME, not by version.
}

// One maiTerm task (docs/tasks.md; Rust `Task` in state/workspace.rs, camelCased). Every field
// is ALWAYS present; absence is a stated `null`, never a missing key (the v0.4 rule).
// `dropped` was added in 0.9 and is not a seventh flavour of done: it is RETRACTED work, and it
// does not satisfy anything that was blocked on it. Count it separately or not at all — never
// inside a completion figure.
type TaskLane = 'backlog' | 'todo' | 'active' | 'blocked' | 'review' | 'done' | 'dropped';
interface MaitermTask {
  id: string;               // uuid. NOT a number and NOT an ordering — there is no short row
                            // number anywhere in maiTerm; agents refer to tasks by title. Don't
                            // invent one.
  title: string;
  detail: string | null;    // markdown body
  status: TaskLane;         // the STORED lane — what an edit writes back. Do not RENDER from it.
  effectiveStatus: TaskLane;// what the desktop RENDERS — the mirror of `effectiveStatus()` in
                            // src/lib/tasks/model.ts, computed desktop-side because the phone
                            // cannot: an unfinished row with any open `blockedBy` dep renders
                            // `blocked` whatever its stored lane; a dep id resolving to nothing is
                            // MET (a deleted prerequisite must not wedge its dependents forever)
                            // UNLESS it is parked on an archived tab anywhere in the window, which
                            // is off the list but not gone. The phone receives neither the
                            // workspace's full list nor that parked set with a tab's rows.
  tabId: string | null;     // assignee tab; null = the workspace's unassigned backlog. A tab id is
                            // NOT durable across a reload — never key anything on it alone.
  tabTitle: string | null;  // the assignee's name resolved on the desktop NOW; null when the tab
                            // no longer exists OR is not designated (its name is gated content).
  blockedBy: string[];      // prerequisite task ids. NAMES ONLY — never derive a lock from these;
                            // `effectiveStatus` already did, with information you don't have. An
                            // id absent from this tab's list is on another tab or the backlog;
                            // GET /tasks resolves it.
  origin: 'human' | 'agent' | 'overlord' | 'imported';   // phone-created rows are 'human'
  createdAt: string;        // ISO 8601
  updatedAt: string;
  workstreamId: string | null;  // the named job within the workspace this row belongs to
  workstream: string | null;    // its display name, resolved now
  // `topicId` was REMOVED in 0.9. It was served for four versions and was `null` in every
  // one of them — no desktop ever set it, because nothing in maiTerm ever wrote the field.
  // A client that read it has lost nothing; one that typed it as required should drop it.
  // 0.9. The progress log, oldest first, always an array (`[]`, never absent). `detail` is
  // the SPEC — what the task is; these are what HAPPENED to it. A row in `blocked` says why
  // here and nowhere else, so render at least the last one wherever you show a blocked
  // task. Capped at 20 by the desktop; `by` is stamped there, not claimed by the writer.
  notes: { at: string; text: string; by: 'human' | 'agent' | 'overlord' }[];
}

// GET /tasks — the whole board. Only workspaces with at least one DESIGNATED tab appear, and
// within one, only rows that are unassigned or assigned to a designated tab: "Make unavailable
// in maiLink" (§7) gates task content exactly as it gates the chat. Task-less workspaces are
// included (so "add a task here" is offerable); the Overlord workspace is flagged, not filtered.
interface TaskBoard {
  workspaces: {
    workspaceId: string;
    workspace: string;
    windowLabel: string;    // Overlord is per WINDOW; this is the key the Overlord surface uses
    windowName: string | null;  // what the human calls that window, null when unnamed — show
                                // this rather than the label, which is "main" or a uuid
    overlord: boolean;      // this is the window's Overlord workspace (docs/overlord.md §11)
    suspended: boolean;
    workstreams: { id: string; name: string }[];
    tasks: MaitermTask[];   // board order
  }[];
}

// Rules both boards agree on — stated once here so neither side re-derives them:
//
// 1. WIRE ORDER IS DISPLAY ORDER. The desktop guarantees it (a human can reorder on the board;
//    that is what comes down). Never sort.
// 2. RENDER FROM `effectiveStatus`, COUNT FROM `status`. Chip, lane, lock icon: `effectiveStatus`.
//    Progress denominator, parked count, hidden-behind-"show parked": the desktop predicate
//    `isInFlight = status !== 'done' && !isParked(status)` on the STORED lane, where parked
//    means `backlog` (src/lib/tasks/model.ts:24-31). Backlog is a parking lot, exempt from EVERY
//    in-flight question, not only progress. So a backlog row with an open dep shows a blocked chip
//    AND is counted parked AND is excluded from progress — same as the desktop. Six lanes; there
//    is no seventh "waiting" value.
// 3. DERIVE NOTHING FROM `blockedBy`. It is names. A client that adds its own lock from it will
//    disagree with the desktop the first time a blocker sits on another tab.
// 4. NEVER expect `subject`. A row carries `title`, never both; the phone's one-release adapter
//    discriminates on that, and its removal is gated on the wire protocol version, not a date.

// GET /chats/archived — recoverable tabs (NOT in GET /chats). Flat across workspaces; group by
// workspaceId client-side. tabId is what POST /chats/{tabId}/restore takes.
interface ArchivedChat {
  tabId: string;
  name: string;             // resolved display label captured at archive time
  workspace: string;        // workspace name
  workspaceId: string;      // owning workspace id
  runtime?: 'claude' | 'codex' | 'gemini';  // persisted from when it was live
  archivedAt?: string;      // ISO-8601; list is sorted newest-first
  cwd?: string;             // restore cwd captured at archive time
}
```

---

## 5. Sending replies, commands & answering prompts

All outbound text rides the **existing injection rails** — the same `write_pty` +
bracketed-paste-then-`\r` path the agent bridge uses (`agentPrompt.ts:36`), behind the same
FIFO mailbox + `deliverable()` gate (`agentDelivery.ts`). maiLink never gets a privileged
shortcut; this guarantees it can't corrupt a TUI mid-prompt.

- **Free-text message / proactive command** (`POST .../message {text, submit:true}`):
  bracketed-paste `text`, settle, send `\r`. If the tab is busy/dormant it **queues** and
  flushes on the next `Stop`/re-init (same as bridge messages). Returns `queued|delivered`.
- **Image attachment** (`POST .../message {text?, images:[{data,ext}]}`): the desktop writes each
  image to a temp file named **`maiterm-mailink-<uuid>.<ext>`** (`mod.rs:974`, via `temp_dir()`)
  and injects the file path(s) followed by `text` on the same rails — the "raw-path inject". Claude
  Code does **not** reliably convert a programmatically-typed path into a native `[Image #N]` chip
  (that's a paste/drag heuristic in the interactive composer), so the path usually stays literal in
  the persisted user turn as `<path…> <caption>`. **The `maiterm-mailink-` filename stem is a shared
  cross-repo contract**: the desktop distiller strips a leading run of these paths from the echoed
  user turn (`transcript.rs` `strip_leading_image_refs`, keyed on that marker) so the persisted echo
  is the bare caption, and the app's `captionFromEcho` strips the same marker for its optimistic
  live bubble — so echo == caption on both the live send and thread re-open. A leading path is only
  stripped if it carries the marker, so ordinary messages that mention a path are untouched. **If the
  temp-file stem ever changes, both sides must change together.**
  **Body-size ceiling: 32 MiB** for `POST .../message` (`MAX_MESSAGE_BODY_BYTES`, `mod.rs`); every
  other route keeps axum's 2 MiB default. Over the ceiling the request is rejected by the extractor
  with **413** *before* the handler runs — so there is no server log line and nothing is injected.
  Budget phone-side by **total** bytes, not image count: per-image size varies far more than the
  6-image cap implies (JPEG photos ~<500 KB, but PNG screenshots have been seen at ~750 KB, and
  base64 inflates ~1.37× on top), so a 6-image batch runs ~3–6 MB and two heavy screenshots alone
  used to exceed the old limit. A 413 on this route means "images too large", not a transient error
  — don't retry it verbatim.
  **SSH tabs**: with a live bridge tunnel, the desktop stages the decoded bytes in the REMOTE
  host's `/tmp` (same `maiterm-mailink-<uuid>.<ext>` stem — the marker contract above holds
  unchanged for echo-stripping) by streaming them over the tunnel's mux socket, then types the
  remote paths; the send returns `delivered` exactly like a local tab, all-or-nothing (a failed
  transfer never types half a batch). `{status:"unsupported", reason:"unsupported_ssh"}` now means
  only "no usable bridge" (tunnel down/disabled, mosh) or "staging failed" — render it as the same
  in-app notice as before; a retry after the bridge reconnects can succeed.
- **Answer a permission/question** (`POST .../respond {choice, prompt_id}`): Claude's TUI
  answers permission with a numeric/selection keystroke (e.g. `1`=yes, `2`=yes+don't-ask,
  `3`/Esc=no). The desktop maps `choice` → the correct keystroke for that runtime and injects
  it **without** bracketed paste (it's a single keypress, not a paste). The
  `pendingPrompt.options` in `ChatDetail` are what the phone renders as buttons; the
  index/label maps server-side so the app never hard-codes TUI key bindings. **`prompt_id` is
  the stale-guard** (multi-phone safety): the server only injects if `prompt_id` matches the
  currently-open prompt, else returns `{ok:false, reason:"stale"}` — so a late-waking phone
  can't approve a prompt that's already been superseded/auto-resolved, and two phones can't
  double-answer. **This keystroke mapping is still the one fragile spot** (depends on the
  agent's current TUI affordance) — so the robust fallback is always available: just send a
  text message (e.g. literally typing "no, use the staging bucket instead").
- **Activate** (`POST .../activate`): for a dormant maiLink-native tab, run the existing
  auto-resume/spawn path (the same machinery clone/bridge use) and `switchTab` to focus it;
  return the resulting state. For a live tab it's a focus + presence no-op.
- **New conversation** (`POST .../new`): create a sibling tab that inherits ONLY where the source
  runs — its SSH command and cwd — and launches a FRESH agent session there (the runtime's bare
  launch command rides the existing auto-resume replay, so the tab comes up connected, in the
  right directory, with a live agent; `/maiterm init` needs no help because the SessionStart hook
  injects it). Explicitly NOT the duplicate path: duplication copies the session-id trigger
  variable — correct for reload and `/branch` forks, and exactly wrong here, since the copy would
  re-render the SAME conversation instead of starting one. Nothing else is inherited: no
  scrollback, notes, shell history, trigger variables or the source's own auto-resume command. The
  source tab is untouched and need not be idle. Works for SSH tabs (it's just another SSH spawn —
  no remote inspection needed). Tab creation happens in the owning window, so the endpoint waits
  for the new tab to exist and returns its id only once it's navigable; the roster picks it up on
  the next `chats_changed`.
- **Interrupt**: inject `\x1b` (Esc) — the documented "human interrupts the agent" gesture. A
  single Esc byte, nothing else — it interrupts a running turn, clears a half-typed line, and
  backs out of a TUI menu (including the resume startup menu), so the button is a safe always-on
  escape hatch regardless of the tab's believed state. `404` if the tab isn't maiLink-available;
  `409` if it has no live PTY (nothing to interrupt — resume it first). Returns
  `{ ok: true, settled: bool }`: **Claude Code does not fire its `Stop` hook on a user
  interrupt** (only on a normal turn completion), so the hook-driven chat state would otherwise
  latch at `active` ("Working") forever after an Esc. Because the interrupt is server-originated,
  the endpoint authoritatively settles the tab's running sessions to idle in the same in-memory
  state every reader consumes (WS + REST both converge on `idle` within a tick — no transient
  event to be clobbered by the poll floor). `settled` is `true` when a running turn was actually
  stopped, `false` when nothing was running. If the Esc didn't land and the agent keeps working,
  the next real tool-use hook flips the state back to `active`, so a spurious idle self-heals.
  **When `settled` is true the endpoint also clears the composer** (Ctrl+L = Claude Code's
  `chat:clearInput`): cancelling a running turn makes CC restore that turn's prompt into the
  composer for editing, and since message injection is a bracketed paste APPENDED to whatever the
  composer holds, the next message from the phone would otherwise land on the restored text and
  submit both as ONE concatenated prompt. Nothing is cleared when `settled` is false, so a Stop
  can't wipe a draft being typed at an idle desktop prompt.

  **The clear waits for the restore rather than assuming a delay.** It first waits for the Esc to
  produce output (that repaint *is* the restore), then for the output to stop, then clears —
  bounded at 3 s. A fixed 150 ms delay used to lose this race in the field: three phone sends,
  each followed by Stop, concatenated into a single prompt on a tab where `settled` was true and
  the clear had definitely fired. No output at all within the cap means nothing was cancelled, so
  the composer is left alone. `composerCleared: bool` reports what happened, which is what
  distinguishes "never cleared" from "cleared and it didn't take" in a merge report. Because it
  waits on a real signal, a Stop that cancels a heavy repaint can hold the response for up to 3 s.
  (This settles maiLink's own view; a *desktop keyboard* Esc bypasses this endpoint and would
  need PTY observation to detect — out of scope here.)
- **Rename** (`POST .../rename`): set the tab title. The title is trimmed and capped at 120
  chars; empty/whitespace-only is `400` (not a way to clear the name). Sets `custom_name` so the
  chosen title pins against later OSC/agent title overrides (same as a desktop rename), persists
  it (survives resume/restart), and updates the live desktop tab strip in every open window.
  Returns the normalized `title` actually stored. The label reaches other phones as a
  `chats_changed` on the next WS tick (≤1.5 s) — re-GET `/chats`.
- **Resume workspace** (`POST .../resume-workspace`): wake the *suspended* workspace that owns
  this tab. Tab-scoped so the phone never has to model workspace ids — the server resolves
  tab→workspace. A **suspended** workspace has its PTYs killed and tabs restore-on-demand, so a
  per-tab Initialize can't work — `/wake` returns `reason:"no-pty"` and `/message` returns `409`.
  The UI should therefore key off `workspaceSuspended`: when `true`, show **Resume workspace**
  instead of Initialize. This endpoint returns `{ ok, resumed }` —
  `resumed:false` (200) if the workspace was already awake (proceed to normal per-tab Initialize),
  `resumed:true` if a resume was kicked off. Suspend/resume is a frontend operation (PTY respawn +
  agent auto-resume live in the desktop app), so the backend signals the owning window to run it;
  it **respawns exactly the tabs that were live at suspend and re-inits their agents**, so after
  resume the tab is live on its own — no separate per-tab Initialize needed. The suspended→awake
  transition reaches every phone as a `chats_changed` (≤1.5 s) — re-GET `/chats` and the tabs now
  report their live state.
- **Wake / per-tab Initialize** (`POST .../wake`): the action to offer whenever a chat reports
  `registered:false`. It is also applied AUTOMATICALLY, with identical rules, ahead of every
  `POST .../message` — so a phone that just sends never has to think about registration.

  **Never send `/maiterm init` as a message to do this.** An unregistered tab is not merely
  untracked: if its agent has *exited*, the PTY is a bash prompt, and anything injected there is
  run as a shell command. The recovery affordance would be executing shell commands in the tab it
  was meant to recover. The remedy depends on whether the agent PROCESS is alive, which the phone
  can't see and shouldn't have to — so the desktop probes the process tree and picks:

  | tab state | remedy | `woke` |
  |---|---|---|
  | registered | none needed | `null` |
  | unregistered, agent alive (or a live ssh hop) | types `/maiterm init`, dialog-safe | `"init"` |
  | unregistered, agent gone, tab has auto-resume | replays the auto-resume | `"resume"` |

  Getting this wrong is destructive in both directions — a resume replay typed into a running
  agent injects junk and nests ssh — so a live agent always takes `init`, even when a resume
  command is also stored.

  **A registered tab is never touched**, which is the point: it may be mid-turn, and typing into
  a running turn is the one thing guaranteed to corrupt it.

  **It's synchronous, with a 45 s budget** (`WAKE_BUDGET_MS`). A local `claude --resume` is
  usually up inside 10 s; an ssh hop plus a remote agent boot can take 20–30 s. Design the UI for
  a long hold rather than a spinner that reads as hung. When the budget expires the wake keeps
  going, so a retry a few seconds later normally succeeds immediately — word `wake-timeout` as
  "still starting up, try again", not as a failure.

  Failures are `200` with a machine-readable `reason` plus a human `detail` (same shape as
  `unsupported` for images — never a "do it on the desktop" deferral):
  - `no-pty` — no live terminal. A **suspended workspace** lands here: resume it first.
  - `no-agent` — the agent exited and there is no auto-resume to restart it. On `/message` this
    means the text was NOT delivered, which is the fix: it would have gone to bash.
  - `wake-timeout` — the remedy was applied but nothing came up in time. Also not delivered.
  - `already-registered` — `/wake` only; nothing needed doing.

  If registration hasn't landed by the end of the budget but the agent process *is* up, a
  `/message` is delivered anyway (`woke` set) rather than lost — being unregistered was never a
  reason a paste couldn't land, and a runtime that doesn't register has no other signal.
- **Mesh Initialize-all** (`POST .../mesh-init`): bring the *Mesh Workspace* that owns this tab
  back to ready, typically after a maiTerm restart. Tab-scoped like resume-workspace (server
  resolves tab→workspace; the `mesh` field on `Chat` says when to offer it). Post-restart, mesh
  members usually show `state:"dormant"` even though their agent processes were auto-resumed —
  what they lost is their maiTerm *registration* (hook liveness + the MCP connection→tab binding
  that routes their outbound mesh sends), restored by `/maiterm init`. The desktop triages each
  member by a process probe: agent still running → types `/maiterm init` into its PTY
  (dialog-safe); agent exited → replays the tab's auto-resume; already-live members untouched.
  The phone does NOT need to distinguish these cases — one tap covers all members, and progress
  arrives as normal `chat_state` events (`dormant` → `active`/`idle`) as each agent re-registers
  (allow ~5–30 s per member; resume replays can take longer). Returns `{ ok, initiated }`:
  `initiated:false` with `reason:"not-mesh"` (workspace isn't a mesh) or
  `reason:"workspace-suspended"` (resume the workspace first — mesh-init needs live PTYs);
  `initiated:true` with `workspaceId` when the pass was kicked off. Per-member Initialize from
  the thread view is `POST .../wake` (above) — the same triage, scoped to one tab.
- **Cancel a queued message** (`POST .../queue/cancel`): pull a message back out of the input
  queue before the agent reaches it. **Offer it only when `queued` holds exactly one entry** — the
  endpoint enforces that too, and the reason is mechanical rather than cautious.

  Reading Claude Code 2.1.220: cursor-up in the chat input calls `popAllEditable`, which recalls
  the WHOLE queue into the composer as one blob. A per-message variant exists (`popEditableAt`,
  driven by a `queueEditIndex` the arrow keys move) but sits behind `CLAUDE_CODE_KB_COHESION_FIXES`,
  unset by default. So with two messages queued, one cursor-up recalls BOTH and clearing the
  composer would silently destroy the message the user didn't pick. With exactly one queued,
  "pop all" and "pop that one" are the same operation. `multiple-queued` (with `queuedCount`) is
  the refusal.

  **It verifies rather than assumes.** After the cursor-up it watches the transcript for the queue
  to actually empty (recalling writes a `popAll` op), and clears the composer only once it has.
  A recall that didn't happen returns `cancelled:false, reason:"not-recalled"` with the composer
  untouched — never a false success, and never a clear that could wipe something maiTerm didn't
  put there. Other refusals: `already-consumed` (the agent got there first — expected, since the
  affordance renders against a queue that keeps draining), `not-registered`, `unsupported_runtime`
  (Claude only). `409` if the tab has no live PTY.
- **Archive / Close / Restore** (tab lifecycle): two DISTINCT real operations, both reversible-ness
  labelled honestly:
  - **Archive** (`POST .../archive`) — RECOVERABLE. The tab leaves the workspace's live tabs into
    its archive; scrollback + restore context (cwd, ssh, auto-resume) are preserved. It drops from
    GET /chats and a `chats_changed` follows (≤1.5 s). Later restorable.
  - **Close** (`POST .../close`) — DESTRUCTIVE, NOT recoverable. The genuine end-conversation path
    (desktop Cmd+W): PTY killed, tab removed, scrollback deleted, any agent bridge torn down — no
    archive entry afterward. The phone MUST confirm before calling this.
  - **Restore** (`POST .../restore`) — reverses Archive. The `tabId` is an ARCHIVED tab (from GET
    /chats/archived), not a live one. Respawns the PTY + replays auto-resume; reappears in GET
    /chats on the next `chats_changed`. Returns the resolved `workspaceId`.
  All three are frontend-driven (serialize/kill/respawn PTYs), so — like resume-workspace — the
  backend emits to the owning window and returns `{ok}` immediately; the observable result arrives
  as the follow-up `chats_changed`. `404` if the tab id isn't a live maiLink tab (archive/close) or
  isn't an archived tab (restore). Archived tabs live outside GET /chats: enumerate them with **GET
  /chats/archived** (flat `ArchivedChat[]`, newest-first; group by `workspaceId` on the phone).

---

## 6. The doorbell (APNs/FCM) — the only internet egress

When an attention event fires (`permission`, or `idle`/done via the Stop hook, or a
question) for a maiLink-native tab **and** no paired device holds a live WS for it:

```
desktop ─POST {push_token, platform, env, cap, tab_id, kind, title}─► relay ─┬─APNs─► Apple  ─► iPhone
                                                                            └─FCM──► Google ─► Android
```
The relay fans out by `platform` (`apns`→JWT/APNs, `fcm`→HTTP-v1/FCM). Same content-light
payload either way; `cap` is the per-device capability (below).

- **Payload is content-light.** No prompt text, no terminal output, no cwd — only the tab
  `title` + `kind` (`permission`/`question`/`idle_done`), which is all the alert renders. Apple
  and the relay learn *that* an agent wants you and which tab, never the prompt. The phone wakes,
  opens the WS over LAN/WireGuard, and pulls the real content.
- `tab_id` drives `apns-collapse-id`/`thread-id` so repeated pings for one tab coalesce.
- `apns-priority: 10` + a time-sensitive alert for permission/question; an `active` alert for
  done/idle. Respect the phone's own mute.
- **`interruption-level` needs BOTH repos, and the relay half is the half you can see.** Sending
  `time-sensitive` is necessary and not sufficient: it does nothing unless maiLink ships
  `com.apple.developer.usernotifications.time-sensitive` in `ios/App/App/App.entitlements` (and
  the App ID has the capability, and the profile was regenerated for it). An unentitled level is
  **ignored, never rejected** — APNs still answers 200, so the desktop's logs look identical
  either way and a Focus mode still holds the alert. **To know whether this works, read that
  plist. Never infer it from this relay's code, which reads like evidence and isn't.**

  *Observed 2026-09-03:* the key was absent, and had been since the doorbell shipped — so
  `permission` had silently never broken through Focus either. Found only by reviewing the
  `question` fix; verified against 1237 local doorbell pushes, zero non-200. maiLink added it the
  same day. This paragraph is a dated observation, not a status: re-read the plist rather than
  trusting it.
- **The relay's copy table is the notification.** No maiLink code path rewrites the alert:
  `@capacitor/push-notifications` does register a `UNUserNotificationCenterDelegate`, but it only
  reads `content.title`/`.body` and never builds a `UNMutableNotificationContent`. iOS renders the
  relay's `body` verbatim and the phone cannot correct it. `permission` → "Needs your approval", `question` → "Needs your answer",
  `idle_done` → "Agent finished", and **an unrecognised kind falls back to "Needs you" at
  time-sensitive**, never to "Agent finished". A kind this relay doesn't know is one the desktop
  grew after it shipped; defaulting to "finished" would announce the opposite of a human being
  waited on. Only `idle_done` is an FYI.

> **The phone needs TWO routes at once — by design.** The doorbell splits across networks:
> the **wake path** (the phone registering its APNs/FCM token, the relay cap mint, and Apple/
> Google delivering the push) needs the **public internet**; the **content path** (the WS pull
> after the phone wakes) needs a route to the desktop (**LAN or WireGuard**). This is exactly
> the normal WireGuard topology — phone on cellular/WiFi for internet **and** the WG tunnel for
> the desktop — so it's not a constraint in practice. A phone on a **LAN-only AP with no
> internet uplink** is one degenerate case: it reaches the desktop fine, but APNs can never
> issue a token, so the chain stalls before any `/push-capability`/`/push-register`. Desktop
> symptom: the paired device's `last_seen` never advances and `push_token`/`push_cap` stay empty.
> Fix: give the phone a link with **both** internet and desktop reach.
>
> **Debugging note — that symptom is ambiguous.** "iOS `register()` called, but the plugin emits
> neither `registration` nor `registrationError`" looks identical for (a) no internet / APNs
> unreachable and (b) a **missing AppDelegate forwarding** of
> `didRegisterForRemoteNotificationsWithDeviceToken` / `didFailToRegister…` into the push plugin
> (the classic Capacitor gotcha — the stock template omits both methods). In the live bring-up it
> was (b), not the network. **Check the app's APNs wiring first** (faster to rule out), then the
> network path.

### 6.1 The relay is shared, multi-tenant infra — **the Flexmark-operated Cloudflare Worker**

maiLink ships as **one published app** (one bundle id, one Apple `.p8`, one FCM project), so
**one** relay serves **every** user — each phone just has its own per-device push token. The
project already operates a Cloudflare Worker (`update-worker/`, `updates.maiterm.dev`); it gains
`POST /push` + `POST /push-capability`, holding the `.p8`/FCM key that can't live safely on
clients. The desktop's built-in default relay URL points here; `Preferences.mailink_relay_url`
is only an optional self-host override.

**Why there is no shared relay key.** Because the relay is multi-tenant, it can't authenticate
desktops with one secret (it would have to ship in every install → extractable → open spam
proxy). Instead the relay holds a server-side `CAP_SECRET` and each phone mints a **capability**:

```
phone ─POST /push-capability {push_token, platform}─► relay ─► {cap = base64url(HMAC-SHA256(CAP_SECRET, "platform:push_token"))}
```

The phone hands `cap` to the desktops it pairs with (via `/push-register`, over the pinned-TLS
LAN channel). The desktop presents `cap` on every `/push`; the relay recomputes the HMAC and
rejects a mismatch (`403`). Properties: `CAP_SECRET` never leaves the relay; a desktop can't
forge a cap for a token it never received from a real phone; rotating `CAP_SECRET` revokes every
cap at once; the relay stays **stateless** (no DB). Possessing the push token is the underlying
gate (tokens are app-private and only ever travel APNs→phone→pinned-TLS→paired desktop).

Relay endpoints (in `update-worker/`):
- `POST /push-capability` — `{push_token, platform}` → `{cap}`. Open mint (rate-limit later).
- `POST /push` — `{push_token, platform, env, cap, tab_id, kind, title}`. `403` on a bad cap,
  `503` if `CAP_SECRET` unset, else echoes the upstream APNs/FCM verdict.
- gateway-by-`env`: only `env:"production"`→`api.push.apple.com`, else the sandbox gateway.

---

## 7. Security & threat model

| Threat | Mitigation |
|---|---|
| Anyone on the LAN hitting the bridge | Bridge is **off by default**; bearer token required; pairing needs the one-time QR code |
| Eavesdropping / MITM on LAN | TLS (self-signed) + **cert pinning** via QR fingerprint |
| Stolen/lost phone | Revoke the device in Prefs → token hash deleted → instant lockout; tokens are per-device |
| Token theft from disk | Token stored hashed server-side; on the phone it lives in the iOS Keychain |
| Replay / pairing-code reuse | Pairing code is single-use + ~120 s TTL |
| Doorbell abuse / data leak via cloud | Relay payload is content-free; relay is stateless; `.p8` never on clients |
| Exposing plain shells / non-agent tabs | Only *agent* tabs (with a detected `runtime`) are ever available; plain shells are never auto-exposed. In designate-only mode the user may still hand-pick a shell via `mailink_native` |
| Reaching a held-back tab by known tab_id | Every tab-scoped endpoint — `context`, `message`, `respond`, `interrupt` (not just list/stream/doorbell) — passes through `is_designated()` and returns `404` for a non-available tab. "Make unavailable in maiLink" is a real gate, not just a visibility toggle |
| Cross-contaminating the IDE/MCP server | maiLink is a **separate listener**; `claude_code/server.rs` stays bound to `127.0.0.1` |
| Injection corrupting a TUI mid-prompt | Same FIFO + `deliverable()`/`isAwaitingHumanInput()` gate as the agent bridge |

Off-LAN access is the user's **WireGuard** tunnel — we never expose the bridge to the public
internet directly, and we don't ship a VPN. The QR simply carries the WG peer IP when remote.

---

## 8. Discovery

QR carries host+port, so zero-config discovery is optional. A nicety for later: advertise
`_mailink._tcp` via mDNS/Bonjour so a paired phone re-finds the desktop after a DHCP IP
change without re-pairing (the cert+token stay valid; only the address moves). Not needed
for v1 (QR re-scan covers it).

---

## 9. Build plan (desktop side)

Phased so each lands independently and is testable without the phone:

- **P0 — Contract lock.** This doc, reviewed by product owner + maiLink agent. ← *we are here.*
- **P1 — Designation + Prefs.** `mailink_native` on Tab/Workspace, the two set-commands, the
  master enable + device list in Preferences, the context-menu/workspace toggles. No
  networking yet. Verifiable purely in the desktop app.
- **P2 — `mailink` module + pairing.** New `src-tauri/src/mailink/`: gated TLS axum listener
  (rcgen self-signed), `/pair`, bearer-token store, `/chats`, `/chats/{id}` + `/context`.
  Test with `curl --cacert`/pinning from a laptop. No push, no WS yet.
- **P3 — WS live channel + actions.** `/ws`, `/message`, `/respond`, `/activate`,
  `/interrupt`, presence/coverage suppression. This is the full chat loop over LAN with the
  app foregrounded.
- **P4 — Doorbell.** Relay route on the Cloudflare Worker + desktop trigger on attention
  events when uncovered + `/push-register`. End-to-end background wake.
- **P5 — Hardening.** Revocation UX, reconnect/backoff, rate-limits, mDNS, Android/FCM
  transport behind the same contract.

P1 is a safe, self-contained first commit. P2+ should land in lockstep with the maiLink app
so the contract is exercised, not just asserted.

---

## 10. Open questions

**For the product owner:**
- [ ] **Launch scope** — iOS-first, or iOS+Android at launch? The maiLink agent is building
      cross-platform (Capacitor), and the protocol supports both; this is purely a
      go-to-market/effort call, not a technical one.
- [ ] **Push key hosting** — confirm **(A) reuse the Cloudflare Worker as a stateless push
      relay** (recommended) vs (B) embed keys vs (C) per-install team key. Note: now hosts
      BOTH the APNs `.p8` and an FCM service-account key (Capacitor → both platforms). (§6.1)
- [ ] **"Activate" semantics** — does activating a dormant maiLink-native tab mean (i)
      resume an existing agent session, (ii) start a fresh agent, or (iii) just focus it if
      already running? (Likely "resume if it has a session, else start" — confirm.)
- [ ] **Transcript fidelity** — is "distilled recent plain-text + structured attention
      events" enough for the chat view in v1, or do you want a cleaner turn-by-turn
      transcript (would need parsing agent output into messages)?
- [ ] **Multiple phones** — expected (you + staff)? Affects presence/coverage and whether a
      reply from phone A should echo to phone B's thread. (Contract already supports N
      devices; just confirm the UX expectation.)

**For the maiLink agent — RESOLVED (v0.2):**
- [x] Stack = **Capacitor + SvelteKit + shadcn-svelte** (cross-platform).
- [x] Cert pinning = **native transport plugin** (iOS URLSession challenge + Android OkHttp
      TrustManager), not JS — owns REST + WS with pinned-fingerprint trust. (§3.1)
- [x] JSON shapes in §4 **agreed** (the v0.2 deltas above are the agreed deltas).
- [ ] Bundle-id + APNs `.p8` + FCM service-account ownership → handed to the relay after
      Darryl signs off on hosting (§6.1).

---

## 11. Status of the desktop implementation

- **P1 — DONE** (`fe95aff`): `mailink_native` on Tab/Workspace + `mailink_enabled` pref,
  set-commands, TS/store wiring, tab "Expose to maiLink" toggle, Preferences section.
- **P2a — DONE** (`1fde520`): `src-tauri/src/mailink/` gated TLS listener (rcgen self-signed,
  persisted, SAN-agnostic), `/heartbeat`, fingerprint pipeline (unit-tested vs `openssl`).
- **P2b — DONE** (`96f49d9`): dev bearer token + `GET /chats`, `/chats/{tabId}`,
  `/chats/{tabId}/context` derived from live `agent_sessions` state. Compiles + unit tests pass.
- **P3 — DONE** (`6da933a`): write path (`POST /message`, `/respond` with prompt_id
  stale-guard, `/interrupt`) + live WS event stream (`GET /ws`).
- **P4a — DONE** (`533ed1f`): production pairing — `POST /pair` (one-time code → per-device
  bearer token, stored hashed/revocable) + `POST /push-register`; auth widened to dev-token
  OR device token; `mailink_create_pairing` command → QR payload. Verified live.
- **🏁 INTEGRATION PASS — PROVEN END-TO-END (this session)** with the maiLink Capacitor app on
  an iOS simulator over pinned self-signed TLS, against a live Claude agent:
  - on-device cert pin validated; `GET /chats`/thread/`context` pulled live;
  - live state (dormant→active→idle) rendered in-app; attention → "Needs you" + pendingPrompt;
  - **write round-trip**: the phone's `POST /respond {choice:"No"}` denied a real Claude
    `Bash(rm …)` — keystroke landed in the agent's TUI; `POST /message` proactive command
    delivered and the agent acted on it;
  - `/pair`→device-token, `/push-register`, WS frames, auth, `fp`==openssl all verified.
- **P4b desktop — DONE** (`9e927f5`): the doorbell **trigger** — a global ~2s loop that, on a
  maiLink-native tab entering attention (permission/idle-done) with no phone WS-covered, POSTs a
  content-free `{push_token, platform, env, tab_id, kind, title}` wake to the relay per paired
  device. Coverage via `AppState.mailink_ws_count`; relay from `Preferences.mailink_relay_url`
  /`_key`. No-op until a relay URL is configured. Relay hosting confirmed: **reuse the existing
  Cloudflare update-worker** (`updates.maiterm.dev`) `/push` route.
- **Doorbell relay `/push` — DONE** (`c35c0eb`, in `update-worker/`): the Cloudflare worker
  `POST /push` route. Fans out — APNs (ES256 JWT minted from the `.p8`, **gateway-by-`env`**:
  only `production`→`api.push.apple.com`, else sandbox) / FCM (HTTP-v1, OAuth2 from the
  service-account JWT). JWTs cached in the isolate global. Response echoes the upstream APNs/FCM
  verdict for desktop logs.
- **Multi-tenant capability auth — DONE** (`641c947` relay, `c27ac0c` desktop): the relay is
  **shared infra for every user of the one published app**, so the per-user `MAILINK_RELAY_KEY`
  is gone. Added `POST /push-capability` (`{push_token,platform}`→`{cap}`, `cap =
  HMAC(CAP_SECRET, platform:push_token)`); `/push` now requires `cap` (403 on mismatch).
  Desktop: `MailinkDevice.push_cap`, `/push-register` accepts `cap`, the doorbell only rings
  devices with BOTH a token and a cap, the relay URL is **baked in by default**
  (`mailink_relay_url` is an optional self-host override), and the Prefs key field is removed.
  Also fixed `set_preferences` to preserve backend-owned `mailink_devices`.
- **Relay deployed + smoke-passed — DONE** (2026-06-28, worker version `edddb56b`): secrets set
  (`CAP_SECRET`, `APNS_KEY_P8`, `APNS_KEY_ID`=`DRWCHZ5M5B`, `APNS_TEAM_ID`=`7HJJ4SQ4TC`,
  `APNS_TOPIC`=`dev.maiterm.mailink`) and `wrangler deploy` shipped. Smoke: mint→`{cap}`; ring a
  fake token with a valid cap → Apple `400 BadDeviceToken` (JWT + sandbox gateway + Workers→APNs
  **HTTP/2** all confirmed working); wrong cap → `403`; `/latest.json` → `200` (update service
  unaffected).
- **🏁 DOORBELL FINALE — PROVEN END-TO-END ON REAL HARDWARE** (2026-06-28, iPhone, locked): a real
  Claude permission on a maiLink-native tab → the ~2s doorbell loop → relay `/push` (HMAC cap
  verified) → APNs sandbox **`200 OK`** → dPhone **lock-screen alert** → tap → deep-link
  `/chat/{tabId}` → WS reconnect over pinned LAN → live prompt rendered → **the human Approved from
  the phone** → `/respond` injected the choice → the real Claude agent executed the command. The
  whole reason-for-being works: agent needs you while you're away → your phone rings → you answer
  from the lock screen → the agent moves. (Bug chain cleared en route: a missing iOS AppDelegate
  APNs-forwarding method blocked token issuance — classic Capacitor gotcha, peer-side fix.)
- **Post-proof, app-side polish (peer):** strip push-debug breadcrumbs; notifications pre-prompt;
  gate initPush on transport-bootstrap (kill the mock-race); prod signing = flip
  `aps-environment`/registerPush `env` to "production" (relay routes by `env`, **no relay change**).
- **Post-proof, desktop/relay (mine):** fold the desktop capability code into a normal maiTerm
  **release** (the dev instance has it; shipped app doesn't — relay deploy is independent). **Android:**
  the relay's FCM `/push` leg is coded+deployed; needs Darryl to provision a Firebase project →
  `wrangler secret put FCM_SERVICE_ACCOUNT` → run the same finale with `platform:"fcm"`.
- **Pairing & device-management UI — DONE**: Preferences ▸ AI Agents ▸ maiLink now has a real
  **"Pair a phone"** button → a QR modal (renders the `mailink_create_pairing` payload via
  `qrcode-generator`, shows the code + host:port + a 120s expiry countdown with regenerate), plus a
  **Paired devices** list (name, platform/env, doorbell-ready badge, paired/last-seen) with inline
  **Revoke**. New backend commands `mailink_list_devices` (sanitized — no token hash/cap) and
  `mailink_remove_device` (idempotent; drops the record so the bearer stops working and the doorbell
  stops ringing it). Closes the last P5 "Revocation UX" gap and replaces the deferred-UI stub.
- **Per-turn source-markdown distillation — DONE** (`6ce7232`): `ChatDetail.transcript` is now real
  per-turn turns read from Claude's session JSONL (`~/.claude/projects/*/<session_id>.jsonl`, by the
  unique session id — no hook change), not the `recent_text()` terminal scrape. assistant `text`→
  `role:"agent"` (source markdown), `tool_use`→`role:"tool"` (compact `Name(arg)` chip), user string→
  `role:"user"`; thinking/tool_result/system-scaffolding skipped. Claude-only; other runtimes keep
  the scrape fallback (no regression). `mailink/transcript.rs`, unit-tested + validated on a real
  942-turn transcript.
- **Live per-turn WS streaming — DONE** (`fa3b971`): supersedes the "still a refinement" note above.
  `stream_new_messages` (`mailink/mod.rs`) runs a 400ms mtime-gated ticker that diffs each designated
  tab's transcript and pushes one `{type:"message", role, text, msg_id, ts}` frame per newly-appended
  turn. Streams `agent`/`tool`/`system`; **never** the phone's own `role:"user"` turns. Frame fields are
  byte-identical to `GET`'s `turns_for_session(sid, 40, Marker)`, so the phone dedups the streamed frame
  and any REST re-fetch to one entry. Latency win (≤400ms vs the old 1.5-2s re-pull), turn-granular by design.
- **Context-compaction divider — DONE** (`3d96159`): a `compact_boundary` entry (`type:"system"`,
  `subtype:"compact_boundary"`, fields TOP-LEVEL — no nested `message`) becomes one `role:"system"` turn
  `Context compacted · <pre> → <post>` (prefix `Auto-compacted` when `compactMetadata.trigger=="auto"`;
  bare label if metadata absent). `msg_id = entry.uuid` so stream + GET dedup; the streamer already passes
  non-user roles, so it pushes live with no streamer change. The app renders `role:"system"` as a labeled
  divider. Same commit drops the injected post-compaction summary (a `user` entry with
  `isCompactSummary:true`, ~12k chars) that `is_system_noise` didn't catch and was leaking as a giant fake
  user message. Adds `fmt_tokens_k` (776k / 1.2M rounding).
- **Codex + agent-prompts pass — DONE** (2026-07-02, four commits):
  - **Runtime-aware `/respond` keystrokes.** Codex's approval overlay is a *variable-length*
    list (2–5 options) where digits select by POSITION — Claude's fixed `1/2/3` could land
    "No" on a "Yes, and don't ask again…" row. Codex answers now inject its stable default
    letter shortcuts (`y`=approve, `a`=approve-for-session, `n`=decline, per codex-rs
    `tui/src/keymap.rs`); digits from the phone are translated, never passed through. Claude
    keeps the numeric menu.
  - **Codex per-turn transcripts + meta.** `~/.codex/sessions/**/rollout-*-<sid>.jsonl`
    (append-only across resumes; located newest-first, path-cached) distills
    `response_item`s: assistant `output_text`→`agent`, genuine user `input_text`→`user`
    (`<tagged>` scaffolding dropped), `function_call`/`custom_tool_call`→`tool` chips
    (`msg_id` = `cx<line>[:<block>]`, stable for stream/GET dedup). `meta` reads the last
    `token_count` — `last_token_usage.total_tokens` over the stated `model_context_window`
    (exactly codex-rs's own gauge; the `total_token_usage` running sum exceeds the window on
    long sessions) — and `turn_context.model`. Session resolution is runtime-aware
    (`codexSessionId` covers the resume-before-init window), so WS streaming, detail, gauge,
    and recency all work for Codex like Claude. Gemini still falls back to the scrape.
  - **Permission cards show WHAT is being approved.** Sessions capture a compact
    `tool_detail` from the PreToolUse `tool_input` (refreshed from Codex's
    `PermissionRequest`, which carries `tool_name`/`tool_input` on the event) → synthesized
    text is now e.g. `Bash(rm -rf ./dist) — approve?`.
  - **Attention/doorbell transition semantics.** Both tickers diff `state|prompt-kind` (chats
    gain an additive `prompt: "question"|"permission"|null` field) and fire only when a
    *previously-observed* tab transitions INTO attention — a tab merely appearing in the
    roster already idle (exposure toggled, restore) no longer pushes a phantom "finished",
    and an AskUserQuestion opening without a coincident permission notification now rings.
    Chat detail's `unread` counts an open ask like the inbox does.
  - **Bridge/mesh envelope filtering.** `⟦AGENT-BRIDGE⟧`/`⟦MESH⟧`/`⟦TOPIC COMPLETE⟧`
    injections are delivered as real user prompts and were rendering as giant fake "user"
    messages flooding every mesh participant's thread — now dropped by the transcript noise
    filter (and excluded from last-turn recency).
  - **The 60s ask deadline (field bug, 2026-07-02; expiry contract revised 2026-07-03).**
    Claude Code fires NO notification hook for an AskUserQuestion (anthropics/claude-code#13830)
    — state stays `active`. The prompt-kind transition above closes that signaling gap;
    `pendingPrompt` gains additive **`asked_at`** (unix ms, stamped at PreToolUse),
    `questions[].options[]` pass through Claude's per-option `preview`, and the transcript
    chip reads `AskUserQuestion(<first question>)` so an expired ask still shows what was
    asked. Late answers fall back to a free-text `/message`.
    **Expiry**: the 60s auto-resolve existed ONLY in CC 2.1.198–2.1.199 (hard-coded); 2.1.200
    made it **opt-in** via `askUserQuestionTimeout` in `~/.claude/settings.json` (user scope
    only: `"never"` default | `"60s"` | `"5m"` | `"10m"`; multiple-choice questions only —
    permission prompts never auto-resolve). So `pendingPrompt` carries an additive
    **`expires_at`** (absolute unix ms of the actual auto-resolve moment, un-buffered) and it
    is **authoritative**: the desktop emits it only when the session's CC build + settings
    actually expire the ask (version gate read from the session JSONL's per-entry `version`
    field + the settings key — `ask_deadline_ms` in `mailink/mod.rs`). **Absent ⇒ the app
    shows NO countdown** and the question stays answerable until the prompt clears. The app
    closes its tappable window at `expires_at − 5000` (keystroke-inject headroom) and then
    routes to the composer; `asked_at` is display-only ("asked 2m ago") — the app derives no
    deadline from it. Unknown version ⇒ no `expires_at` (a false countdown expires a live
    question; a missing one degrades safely to stale-guard + composer fallback).
- **Two findings (notes, not blockers):** (1) `/message` bracketed-paste is correct for an
  agent TUI but leaks into a bare shell — fine for the intended use; (2) the *first*
  permission (for `initSession` itself) can't be tab-attributed since the session→tab mapping
  happens behind it (surfaces only in a dev/prod dual-instance setup).
- **Known refinements (not blocking):** WS is a ~1.5s internal poller (push-from-hooks later);
  real prompt text/options + stable `prompt_id` need deeper hook capture (prompt lives in the
  TUI, not `agent_sessions`); real `lastActivityTs`/`unread`; question-attention over WS; live
  `message`-over-WS echoes (transcript turns now distilled — see above).

---

## 12. v0.3 — Topic threads & the unified ask contract

This section is the **canonical contract** for the topic-threaded surface. It supersedes the
chat-centric shapes of §2.1 (designation), §4 (wire), §5 (respond), and §8 (discovery) where
they conflict; the transport, TLS, pairing, and doorbell mechanics (§3, §6) are unchanged.

**Why.** maiTerm's Mesh Workspace is **topic-native**: agents converse in topic-scoped threads
(`MeshTopic` — owner, participants, turn count, open/complete, normalized-label dedup). Per-tab
`/chats` leaked an implementation detail (one tab = one session) into the UI. A **thread = a
conversation** is the right model for a chat app, so the app unifies on one `thread` concept and
renders it once.

**Single ask channel (no double-messaging).** The only "human needs to answer" signal is the
agent's **native** prompt — Claude `AskUserQuestion` (structured multiple-choice elicitation) or a
permission prompt — which maiTerm already tracks as `isAwaitingHumanInput`. The desktop side has
**removed** the old agent-authored "status note / NEEDS DECISION" channel and instructs agents to
ask via `AskUserQuestion` only (never print the question, never write a note). One ask in → one
`PendingPrompt` → one card → one `/respond` out. On desktop the same signal raises a scoped
toast + deep-link; on the phone it's the WS `attention` + doorbell.

### 12.1 Canonical TS types (adopted from the maiLink app side)

```ts
export type Runtime = 'claude' | 'codex' | 'gemini';

/** A participating agent in a thread. id is tabId-derived (stable across resume/fork) but is NOT the thread key. */
export interface Participant { id: string; name: string; runtime: Runtime; meta?: AgentMeta; }

/** Per-agent telemetry strip (thread header). All fields optional; the gauge is driven by contextPct. */
export interface AgentMeta {
  model?: string;         // normalized display name: "Opus 5", "GPT-5-codex", "Gemini 2.5 Pro"
  effort?: string;        // Claude reasoning tier: low | medium | high | xhigh | max. Read from the
                          //   transcript's top-level `effort` field (same assistant line as usage).
                          //   OMITTED for effort-less models, non-Claude runtimes, or before the first turn.
                          //   ALSO omitted permanently once a session has been RESUMED: Claude Code
                          //   stops writing the field at the resume boundary and never resumes it,
                          //   so any long-lived tab drifts into this state and stays there. There is
                          //   no other current-effort record in the transcript, and the desktop will
                          //   NOT backfill from the newest line that had one — that value predates
                          //   the resume and can be wrong (measured: a tab on "high" would report
                          //   "medium"). Absent is honest; a stale badge looks identical to a live
                          //   one. Render the unknown state as an affordance to SET the effort,
                          //   never as a value.
  contextPct?: number;    // 0–100, normalized — the always-present field
  contextUsed?: number;   // token detail for the "142k / 1M" readout
  contextLimit?: number;  // model-dependent (1,000,000 for [1m] variants, else 200,000)
}

export type ThreadKind = 'topic' | 'solo';
export type ThreadState = 'active' | 'idle' | 'permission' | 'dormant';

/** Inbox row. GET /threads -> ThreadSummary[] */
export interface ThreadSummary {
  thread_id: string;            // canonical key everywhere (replaces tabId)
  kind: ThreadKind;             // topic = N participants, solo = lone agent tab
  label: string;                // topic label or solo tab title
  owner: string;                // owner participant id
  participants: Participant[];  // drives attribution chips (runtime glyph)
  workspace: string;            // grouping
  state: ThreadState;
  unread: boolean;
  lastActivityTs: number;
  preview: string;
}

/** GET /threads/{thread_id} -> ThreadDetail */
export interface ThreadDetail extends ThreadSummary {
  transcript: Turn[];           // ONE ts-ordered authored list, all participants interleaved
  goal?: AgentGoal;             // the /goal condition being enforced (§4.3) — Claude, GET only
  pendingPrompt?: PendingPrompt;
}

export interface Turn {
  msg_id: string;
  thread_id: string;
  author?: Participant;         // absent => the human/user
  role: 'agent' | 'user' | 'tool' | 'system';
  kind?: 'terminal_snapshot' | 'peer_message' | 'goal_status';  // typed turns (see below); absent => distilled turn
  goal?: {                      // present iff kind === 'goal_status'
    event: 'set' | 'blocked' | 'met' | 'failed' | 'cleared';
    condition: string;
  };
  peer?: {                      // present iff kind === 'peer_message'
    direction: 'in' | 'out';    // the only required field
    name?: string;              // peer's role name; ABSENT on a 1:1 bridge with nothing to name —
                                //   render "peer message sent/received", not a placeholder
    topic?: string;             // mesh topic label; absent on a 1:1 bridge
  };
  text: string;                 // source markdown (for kind:"terminal_snapshot", raw newline-delimited grid text, NOT markdown)
  ts: number;
  queuedAt?: number;            // role:'user' only. Present when the message was typed while the
                                //   agent was MID-TURN: Claude Code queues those and writes no
                                //   `user` turn for them at all — only a `queued_command`
                                //   attachment, at DRAIN time, stamped with the ENQUEUE time.
                                //   `ts` is therefore the drain position (so the message sorts
                                //   after the work it waited on, not above it — otherwise it
                                //   reads as answered before it was sent) and `queuedAt` carries
                                //   the true send time. A client can show "sent 15:25, answered
                                //   15:26"; ignoring it is fine.
}

// kind:"terminal_snapshot" — emitted for tabs with no locatable JSONL (a pruned local session,
// Gemini, a plain shell, or an SSH tab whose transcript mirror is unavailable — see below). It is a
// single system turn holding a raw scrape of the tab's live terminal grid (last ~40 rows,
// newline-delimited, may contain TUI chrome), with a STABLE msg_id (`ctx_<tabId>`) that is
// re-scraped on every GET. Render it preformatted (white-space: pre-wrap; overflow-wrap:
// break-word), badged as a live terminal snapshot, and treat it as ONE replaceable block — not
// appended history. Older clients can sniff the `ctx_` msg_id prefix for the same signal. Dormant
// tabs (no live PTY) omit it entirely → empty transcript → "no messages captured yet".
//
// SSH Claude tabs (v2 transcript mirror): the desktop mirrors the session's REMOTE JSONL into a
// local shadow file (offset-tracked `tail` fetches mux'd over the SSH bridge tunnel's maiTerm-owned
// ControlMaster socket; hook events are the fetch trigger, plus a slow keep-fresh tick while a WS
// client is connected). With a healthy mirror an SSH Claude tab serves REAL distilled turns —
// indistinguishable on the wire from a local tab (same GET shape, same `message` WS streaming, same
// per-agent meta) — and the snapshot turn does not appear. If the mirror can't fetch (tunnel down,
// bridge disabled), the tab degrades to exactly the snapshot fallback above. No client-side changes
// are required either way. Codex/Gemini SSH tabs always use the snapshot path.

// kind:"peer_message" — an agent-to-agent exchange (maiTerm Agent Bridge / Mesh Workspace),
// surfaced so a peer conversation is visible in the thread instead of silently absent. Render as a
// THIN notification-style row ("peer message received from X" / "sent to X"), not a message bubble:
// it is neither party of this thread's conversation. `role` is always "system" for this reason —
// and because the WS streamer never sends `role:"user"` frames, so a user-roled peer turn would be
// missing on the live path and appear only on the next GET.
//
//   * direction:"in"  — a message this agent RECEIVED. maiTerm delivers these as real user prompts
//     wrapped in a ⟦AGENT-BRIDGE⟧ / ⟦MESH⟧ envelope; the desktop strips the routing header so `text`
//     is the peer's actual message body. (Un-stripped, they used to render as giant fake "user"
//     messages, so they were dropped entirely — hence previously invisible.)
//   * direction:"out" — a message this agent SENT, from its `sendToBridgedAgent` call. This turn
//     REPLACES that call's tool chip; the same send never appears twice. `text` is the message sent.
//
// Only genuine peer traffic is tagged: bridge openers, ⟦TOPIC COMPLETE⟧ notices and disconnect
// notices remain filtered as scaffolding, and human/subagent messaging tools (postCommsReply,
// startCommsThread, SendMessage) keep their ordinary tool chips. Emitted identically on GET and on
// the WS `message` event, which carries `kind` and `peer` alongside the usual fields.

// kind:"goal_status" — one /goal state change, in the place in the conversation where it happened.
// The thread-level `goal` field (§4.3) says where things STAND; these rows are the history of how
// it got there, and on a goal that has been turned around a few times they read as a genuine
// progress log. Render as a thin marker row like peer_message, not a message bubble; `role` is
// always "system" for the same two reasons (neither party of the conversation, and the WS streamer
// drops role:"user" frames).
//
//   * event:"set" / "cleared" — the operator installing or removing the goal. No verdict prose
//     exists for these, so `text` is the condition itself.
//   * event:"blocked" — the judge refused to let the turn end. `text` is its explanation of what is
//     still outstanding, which is the single most useful thing in the transcript when you are away
//     from the desk. NOT an ending: the agent was sent straight back to work.
//   * event:"met" / "failed" — the goal was satisfied, or ruled impossible. Terminal either way.
//
// `event` is deliberately finer-grained than the thread field's `state`: a row is a moment
// ("blocked"), the field is a condition ("active").

export interface AskOption { label: string; description?: string; }
export interface AskQuestion {
  header: string;               // short chip, e.g. "Auth method"
  question: string;
  multiSelect: boolean;
  options: AskOption[];
  allowOther: boolean;          // the "Other" free-text path
}

export interface PendingPrompt {
  prompt_id: string;            // stale-guard on /respond
  thread_id: string;
  kind: 'permission' | 'question';
  asked_by: Participant;        // card header + doorbell line
  respondable: boolean;         // permission:true; question:true (selector injection landed, §12.3)
  // permission shape:
  text?: string;
  options?: string[];           // e.g. ["Yes","Yes, don't ask again","No"]
  // AskUserQuestion shape:
  questions?: AskQuestion[];
}

export interface RespondRequest {
  prompt_id: string;
  choice?: string;              // permission: chosen option label
  answers?: Array<{             // AskUserQuestion: aligned to questions[]
    selected: string[];         // multiSelect => >1
    other?: string;             // when user chose "Other"
  }>;
}
export interface RespondResponse { ok: boolean; reason?: string; } // "stale" | "not_respondable"

// WS server->client
export interface WsAttentionEvent {
  type: 'attention';
  thread_id: string;
  kind: 'permission' | 'question' | 'idle_done';
  asked_by: Participant;
  summary: string;
  prompt?: PendingPrompt;       // present for permission/question
  ts: number;
}
// WsMessageEvent / WsChatStateEvent gain thread_id + author analogously; chats_changed -> threads_changed
```

### 12.2 REST/WS deltas

- `GET /threads` → `ThreadSummary[]` (supersedes `/chats`). `GET /threads/{thread_id}` →
  `ThreadDetail`. `/chats*` may remain a thin alias during migration; `thread_id` is canonical.
- WS event `threads_changed` replaces `chats_changed`. `attention` and `message`/`chat_state`
  events carry `thread_id` (+ `author` on message turns).
- `POST /respond` takes `RespondRequest`; returns `RespondResponse` with `reason:"stale"`
  (prompt_id no longer current) or `reason:"not_respondable"` (a race against a `respondable:false`
  prompt — fails cleanly, no dead button).
- **Doorbell** context (§6) gains `thread_id` + `asked_by` so the notification tap deep-links to the
  thread (not a tabId).

### 12.3 Desktop-side mapping & staging (maiTerm)

- **threads** ← a `kind:"topic"` thread is one `MeshTopic` (id→`thread_id`, label, owner/participants
  by tabId-derived `Participant`); a lone agent tab wraps as `kind:"solo"`. `tabId` is the
  participant `id`, never the thread key.
- **transcript** ← per-turn distillation already exists (`mailink/transcript.rs`, §11); add
  `author` (the participant) + `thread_id` per turn and interleave participants by `ts` for a
  topic thread. The app synthesizes the visual grouping; no server-side thread view.
- **PendingPrompt** ← captured from the **PreToolUse hook**, which carries the full `tool_input`.
  **IMPLEMENTED (desktop):** `AskUserQuestion`'s `tool_input` is stored on the session
  (`AgentSessionInfo.pending_question`, set on PreToolUse / cleared on PostToolUse+Stop) and
  served by maiLink as a structured `pendingPrompt.questions[]` = `{header, question, multiSelect,
  options:[{label, description}], allowOther:true}` with `kind:"question"`, `thread_id`,
  `respondable:false`. Permission stays synthesized (`kind:"permission"`, `respondable:true`,
  `options:["Yes","Yes, don't ask again","No"]`). `asked_by` for solo threads = the tab's agent
  (the app's adapter fills it today; native field follows with `/threads`).
- **respondable staging:**
  - `permission` → `respondable:true` **now**. The permission `/respond`→TUI-inject path is already
    proven end-to-end on hardware (§11 doorbell finale) — converging to threads must NOT regress it.
  - `AskUserQuestion` → `respondable:true` **now** (was staged false). `drive_question_answers`
    (`mailink/mod.rs`) drives the TUI selector by keystroke; mechanics pinned live against Claude
    Code 2.1.x. The selector is a tab row `[Q1..Qn][Submit]`, arrow-navigable only (the shown 1..n
    are labels, not digit keys), highlight starts at row 0:
    - **single-select:** ↑/↓ to the row, Enter selects AND advances to the next tab.
    - **multiSelect:** Space toggles each row (live), then → advances the tab.
    - **Other free-text:** the "Type something" row (index = option_count) is a live inline input —
      navigate to it and TYPE directly (no Enter-to-open); single-select then Enter-advances.
    - **submit:** a lone single-select question submits on its own Enter; every other form lands on
      the Submit tab and takes one final Enter.
    Verified e2e (agent echoed exact answers): single-select, single-select+Other, multiSelect,
    mixed multi-question. **Best-guess pending device validation:** multiSelect + Other in the same
    question (type → Enter-commit → →), because the active input swallowed the raw → in probes.
    No "answer on desktop" fallback — every phone-reachable shape must work in-app.
- **answer field names — PINNED:** the emitted question fields are exactly
  `{header, question, multiSelect, options:[{label, description}], allowOther}` (§12.1, verbatim
  from Claude's `tool_input` + synthesized `allowOther:true`); the answer is
  `RespondRequest.answers[]` aligned to `questions[]`, each `{selected: string[], other?: string}`.
  No rename needed app-side — §12.1 is canonical. (`/respond` write path for questions is the
  remaining desktop item: translate `answers[]` → the TUI selection, then flip `respondable:true`.)
- **meta (per-agent telemetry) — IMPLEMENTED (Claude + Codex):** `model` + `contextPct`/
  `contextUsed`/`contextLimit`, read from the session's transcript file
  (`mailink/transcript.rs`, dispatched by runtime):
  - *Claude*: the last JSONL line carrying `message.usage`, summed
    `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`, over a
    model-dependent limit, and `model` normalized from `message.model`
    ("claude-opus-5" → "Opus 5"). The limit is 1,000,000 for `[1m]`/`-1m` ids — but note the
    transcript's `message.model` NEVER carries that marker (Claude Code exposes it only on the
    statusLine input, which maiTerm doesn't receive), so a 1M session and a 200k one report the
    same bare id. The desktop therefore infers it: an allowlist of ids whose 1M variant is the
    one in use (`ASSUMED_1M_MODELS` in `mailink/mod.rs`), plus a backstop that treats any
    session already past 200,000 tokens as 1M. Clients should treat `contextLimit` as
    authoritative and not re-derive it from the model name.
  - *Codex*: the rollout's last `token_count` — `last_token_usage.total_tokens` over the
    stated `model_context_window` — and `turn_context.model` ("gpt-5.5" → "GPT-5.5").
  Emitted on the `/chats` object, in `chat_detail`, and on the WS `chat_state` event (so the
  gauge steps live per turn). `effort` is omitted (only in Claude Code's statusLine payload,
  not received). Gemini tabs get no `meta` (no transcript source yet).

## 13. v0.5 — Overlord on the phone, and phone writes

The Overlord (docs/overlord.md) is the per-window supervisor: escalations that need a human,
propose-mode rule proposals awaiting approval, agent reports, outstanding directives, ritual
progress. v0.5 puts that board on the phone — read, and the four things a human does to it
(dismiss, approve, answer, drive) — and lets the phone write maiTerm tasks (§4.3 `MaitermTask`).

### 13.1 The one fact that shapes everything: the engine lives in the webview

The engine is a **frontend store, per window**. Rust holds none of its state, and it cannot ask:
an occluded WKWebView throttles its timers, so a request that waited on the webview would hang
exactly when a phone is in use — desktop asleep. So:

- **Reads are served from a MIRROR.** The engine publishes a snapshot to Rust on change
  (debounced ~750 ms, and after every 5 s engine tick); Rust gates it, stamps it, and serves the
  last one. The phone never waits on the desktop to read.
- **Task writes go to Rust.** `Workspace.tasks` is Rust state; Rust writes it, saves, and tells the
  desktop's store to replace its copy. Synchronous, confirmable, works while the screen is asleep.
- **Overlord actions cross into the webview**, because there is nowhere else the engine's state
  exists. They answer `{accepted, confirmed}` (13.4), never a bare ok.

### 13.2 Reading the board — `GET /overlord`, WS `overlord`

```ts
// GET /overlord → { windows: OverlordWindow[] }   ([] until a webview has published)
interface OverlordWindow {
  windowLabel: string;        // Overlord is per WINDOW; the key every action route takes
  windowName: string | null;  // what the HUMAN calls this window (titlebar), null when unnamed.
                              // Show it in place of the label, which is "main" or a uuid.
                              // Restamped on every publish, so a rename lands within a tick
  version: number;            // STRICTLY monotonic per window, across desktop restarts too (clock-
                              // seeded, ms-scale — not a small counter). A phone may guard on it
                              // and drop anything older than it holds; a restart never hands it
                              // a smaller number. What the WS ticker diffs on
  asOf: number;               // unix ms, DESKTOP clock, when the engine BUILT this snapshot
  receivedAt: number;         // unix ms, desktop clock, when Rust stored it
  running: boolean;           // Overlord is ENABLED in this window. Every window's engine ticks
                              // regardless (a no-op when disabled), so this is the preference,
                              // not the ticker — a default install publishes running:false
  escalations: Escalation[];  // HUMAN-addressed only (see below)
  proposals: Proposal[];
  agentReports: AgentReport[];
  outstandingDirectives: OutstandingDirective[];
  ritualProgress: { tabId: string; ruleName: string; step: number; steps: number; startedAt: number }[];
  spentTabs: { tabId: string; name: string; done: number; parked: number; lastActivity?: number }[];
  pendingRuleChanges: { id: string; tabId: string; rationale: string; changes: unknown[] } | null;
  lastScan: unknown | null;   // ScanSummary — render if you know it, ignore if not
  rules: { id: string; name: string; enabled: boolean; appliesTo: string[] }[];
                              // what the desktop's "Run an Overlord rule on this tab" menu
                              // offers, RESOLVED to tab ids. The desktop predicate is three
                              // things at once (the tab is an agent tab — terminal, has a
                              // runtime, not exempt; the rule has a runnable sequence; its
                              // scope is global or contains the tab's workspace), so it is
                              // resolved here rather than sent as a scope you would re-match
                              // and drift from. Disabled rules ARE included, sorted last, as on
                              // the desktop — `enabled` says which. A rule that applies to no
                              // tab you can see is omitted entirely, and the whole list is `[]`
                              // when `running` is false: Overlord is off by default, and the menu
                              // does not exist on that desktop either. Fire with
                              // POST /overlord/{windowLabel}/rules/{id}/fire {tabId} — and only
                              // for a tabId in that rule's own `appliesTo`. The desktop enforces
                              // it (a pair outside scope is refused `no_rule`, `accepted:false`),
                              // because typing a workspace-pinned rule into another workspace's
                              // tab is what the scope field exists to prevent.
  agentTabIds: string[];      // terminal tabs in this window's Overlord WORKSPACE — the
                              // supervisor's own conversation. EMPTY IS MEANINGFUL: the window
                              // has no Overlord workspace, or its tabs are not available to the
                              // phone (see below). Say so on screen; do not render an empty
                              // section as if there were nothing happening.
}
interface Escalation {
  id: string; ts: number; tabId: string; workspaceId: string; ruleId: string | null;
  kind: 'step_timeout' | 'blocked' | 'directive_unacked' | 'agent_report';
  detail: string; taskId?: string; read: boolean;
}
interface Proposal {
  id: string; ruleId: string; ruleName: string; tabId: string; tabName: string;
  workspaceId: string; workspaceName: string; preview: string; stepCount: number; createdAt: number;
}
interface AgentReport {
  tabId: string; kind: 'ready' | 'ack' | 'status' | 'escalate';
  state: 'working' | 'blocked' | 'done' | 'idle'; summary: string; task?: string; ts: number;
}
interface OutstandingDirective {
  id: string; ruleId: string | null; tabId: string; stepIndex: number; text: string; sentAt: number; acked: boolean;
}
```

```
{ "type": "overlord", "windowLabel": "main", "window": OverlordWindow, "ts": 0 }
                        // a window's snapshot version moved — the WHOLE snapshot, inline, full
                        // replace (a signal-then-GET would double the window in which the phone
                        // acts on something stale). Baseline on connect for every window; then
                        // only on change. `window: null` = that window closed — a stated absence.
```

**Rules:**

- **`asOf` is load-bearing — render its age.** The desktop may have slept for an hour; this is
  the only field that says so. Staleness is **`(ts − asOf) + elapsed since receipt`**, never
  `phone.now − asOf`: `ts` and `asOf` are the same desktop clock, and phone-clock skew is not
  age. A snapshot with no visible staleness is absence read as a claim. **An awake desktop
  republishes every ~5 s whether or not anything changed** (version moves, a frame goes out),
  so an `asOf` that stops advancing means asleep or off — a quiet board is not a stale one.
  (A first draft skipped unchanged publishes; that froze an idle board's `asOf` at launch and
  made a quiet desktop indistinguishable from a sleeping one.)
- **Only human-addressed escalations cross.** The engine also raises agent-only kinds
  (`drive_reply`, `permission_stuck`, `task_handoff`, `task_dropped`, `rebind_failed`); the
  desktop deck hides them because nobody can dismiss them. On the phone they would light a badge
  nothing clears. The frontend publishes `humanEscalations`, and the union above is the whole
  list the phone can receive.
- **Designation gates the mirror.** Every row that names a tab (`escalations`, `proposals`,
  `agentReports`, `outstandingDirectives`, `ritualProgress`, `spentTabs`) is dropped desktop-side
  when that tab is not designated — an escalation's `detail` is agent text. So are the two
  non-array carriers: `pendingRuleChanges` becomes `null` when its `tabId` is not designated (its
  `rationale` is agent prose), and `lastScan.silent` keeps only designated tab ids. **A window
  with no designated tab at all is ABSENT from `windows`**, not present as an empty board — its
  engine still runs and publishes, but nothing in it can be opened from the phone. Rows naming no
  tab (`tabId: ""`) are about the window and stay. `needsAttention` for a chat is derived from
  this snapshot by `tabId`; there is deliberately no per-chat flag on `/chats`, because an
  escalation can name a tab that is not a chat, and those belong in the Overlord view.
  `rules[].appliesTo` and `agentTabIds` are gated the same way, and a rule left applying to
  nothing visible is dropped rather than offered as a menu entry that would fail.
- **The Overlord agent's own tab is usually, but not always, reachable.** It lives in the
  workspace flagged `overlord: true` (which `GET /tasks` also flags), alongside a `board` tab
  that is never a chat — only terminal tabs are. With the default `mailink_expose_all`, the
  agent's tab becomes available as soon as it has a detected runtime, so normally it IS there.
  It is NOT when expose-all is off and nobody marked it maiLink-native, when the human excluded
  it, or before the agent has started. `agentTabIds` is the answer for a given window; don't
  infer availability from the workspace flag alone.
- **The doorbell rings for a new escalation.** Each publish diffs escalation ids against the
  previous snapshot; a new one, with no phone holding the WS, rings `kind: "escalation"` with the
  tab's title (or "Overlord" for a window-level one). The relay's copy table did not know that
  word when this shipped and falls back to "Needs you" at time-sensitive — the fallback
  direction chosen in §6.1, earning its keep.

### 13.3 Writing tasks — `POST /tasks`, `POST /tasks/{id}`

| Method + path | Body | Returns |
|---|---|---|
| `POST /tasks` | `{ tabId, workstream?: string, tasks: [{ title, detail?, status?: TaskLane, assign?: boolean }] }` | `{ tasks: MaitermTask[] }` — one row per spec |
| `POST /tasks/{id}` | `{ status?, title?, detail?: string\|null, tabId?: string\|null, workstreamId?: string\|null }` | `{ tasks: [MaitermTask] }` |
| `POST /tasks/{id}/start` | `{}` | `{ accepted, confirmed, result?: { started, told, task } }` — see below |

**Setting a lane and telling an agent are different acts, and only a human may do the second.**
`POST /tasks/{id} {status:"active"}` is silent, permanently — that is the path an agent uses to
mark its own row as it picks work up, and a notice inferred from the transition would have every
agent type "please pick up this task now" at itself, mid-turn, about the thing it is already
doing. The board's "Do it" is therefore its own verb:

- `POST /tasks/{id}/start` moves the task to Active **and tells the agent**, exactly as the
  desktop board's button does. It crosses into the webview (the notice is TYPED into the tab), so
  it answers the §13.4 `{accepted, confirmed}` shape, not a row.
- **`result.told` is the point of the call.** `"tab"` — typed into the owning tab, it knows.
  `"agent"` — the tab could not be typed into (mid-turn, at a prompt, not mounted), so Overlord
  holds a handoff and will relay it. `"nobody"` — it is Active and *no one was told*: the task is
  unassigned, or there is no supervisor to relay through. Say which; "Active on the board" and
  "the agent has been told" are different facts, and a button implying the second while doing only
  the first is how a task sits Active for an hour with nobody working on it.
- **`told: "nobody"` has THREE causes with three different remedies, and `reason` says which** —
  in words, deliberately not as a fourth enum value: the distinction is known at the point of the
  answer, `reason` already exists in this envelope, a client that ignores it still behaves
  correctly, and a new enum member would cost every client a branch and an exhaustiveness check
  forever for something a sentence covers. The three: the task is **unassigned** (claim it to a
  tab and the call means something); the owning tab is **Overlord-exempt**, so nothing will ever
  relay it and the remedy is to message the tab directly; or the tab was **unreachable just then
  with no supervisor available**, where trying again later may work. Render `reason` verbatim
  after your own fixed sentence. All three are honest — the task is Active and nobody was told —
  and only the third is worth retrying.
- **`result.task` is the row after the call** — the same `MaitermTask` the other writes return,
  so patch your model from it rather than assuming. It is load-bearing for an unassigned row: the
  WS `tasks` event is keyed by tab, so a backlog task's move to Active reaches you through no
  other channel and would otherwise sit in its old lane until a manual full `GET /tasks`.
- `404` for a task the phone cannot see, same rule as the patch. A task that vanishes between the
  route resolving it and the desktop handling it answers `accepted: false` with a reason — never
  `told: "nobody"`, which would be indistinguishable from a real start.

- **Synchronous.** Rust writes `Workspace.tasks`, saves, then answers. Plain HTTP status — no
  `accepted/confirmed` here, because nothing crossed into the webview. Works while the desktop
  screen is asleep.
- **Idempotent by normalized title within the tab** — the desktop's `findDuplicate` (model.ts),
  tier for tier: (1) the same tab's row in the same workstream; (2) the same tab's row recorded
  loose and restated under a job, or the reverse when unambiguous — filed under the job; (3) an
  UNFINISHED row released to the backlog when its tab closed, in the same grouping — adopted by
  the tab. A `done` backlog row is never resurrected by a restated title; a released twin of a
  title another tab still owns is never re-owned by that tab.
- **Known narrow race, not fixed:** a desktop drag and a phone write within the same IPC hop
  can each persist a whole list the other didn't see; the desktop's edit or the phone's row
  loses for one edit cycle. Versioned persistence would close it; at this user base it is noted
  rather than built.
- `assign` defaults true: the row is this chat's. `false` parks it in the workspace backlog
  (`tabId: null`). `workstream` is a NAME — reused by normalized name or created.
- `origin` is `"human"`. The phone is the human.
- In a patch, absent means leave alone; `null` means clear. `tabId` may only be set to a
  designated tab in the same workspace; `workstreamId` must exist there.
- Errors: `404` a row the phone cannot see (unknown, on an excluded tab, or a backlog row in a
  workspace with nothing designated — one answer for all three, on purpose); `400` a lane outside
  the seven, an empty title, or a target outside the workspace or the gate.
- The desktop's tasks store is told by event (`mailink-tasks-changed`) to replace its copy —
  without that, its next whole-list persist would clobber the phone's row. Implementation detail,
  but it is why this path is safe.

### 13.4 Overlord actions — `POST /overlord/{windowLabel}/…`

| Path | Body | Runs |
|---|---|---|
| `escalations/{id}/dismiss` | `{}` | `dismissEscalation` |
| `proposals/{id}` | `{ action: 'approve' \| 'dismiss' }` | `approveProposal` → `result.outcome: 'started' \| 'stale' \| 'permission'` (a proposal is a snapshot — `stale` is an outcome, not an error) / `dismissProposal` |
| `rule-changes/{batchId}` | `{ approvedIdx: number[] }` | `resolveRuleChanges` |
| `drive` | `{ tabId, kind?: 'process' \| 'slash', text }` | `driveTab` — types into the tab with the human's authority THROUGH the engine (ledgered; the reply harvest sees it). Designated tabs only |
| `rules/{ruleId}/fire` | `{ tabId }` | `fireRule` |
| `recover` | `{ tabId }` | `recoverTab` — `result.sent` means TYPED, not recovered (docs/overlord.md) |

**Every answer is:**
```ts
{ accepted: boolean; confirmed: boolean; result?: unknown; reason?: string }
```
- `accepted:true, confirmed:true` — the window ran it; `result` is the engine's own answer.
- `accepted:false, confirmed:true` — the window REFUSED (`reason`: exempt tab, stale, …).
- `accepted:true, confirmed:false` — the window did not answer within 15 s. **Its screen may be
  asleep and the action may still land when it wakes.** Render "sent, not confirmed" and let the
  next `overlord` snapshot settle it. Never success, never failure — the `recoverTab` lesson,
  `sent ≠ done`.
- `accepted:false, confirmed:false` — the desktop dropped it.

Retire-spent-tab, triage and checkpoint are desktop verbs and are deliberately not here.

### 13.5 Version on the wire — `GET /heartbeat`

`{ ok, now, server_name, fp, protocolVersion: "0.9" }`. The second breaking change in a week
found there was no version anywhere on the wire. A client gates its compatibility shims on this,
not on a calendar; absent means pre-0.5.

**Every wire change bumps it, additive ones included.** A field that appears without a bump makes
two desktops answer the same version while serving different shapes, which is the one question the
field exists to answer. Learned the expensive way: `windowLabel`, `rules` and `agentTabIds` were
added under an unchanged `"0.5"`, a client reasonably typed them as required, and its Overlord
screen threw against a desktop that predated them — on a phone, a route that dies inside a pushed
layer leaves no way back, so "the Overlord button does nothing and now its neighbours don't either".

| Version | What a desktop reporting it guarantees |
|---|---|
| absent | pre-0.5: `tasks` is Claude's session board (`AgentTask`), no `/tasks`, no Overlord |
| `0.5` | §4.3 `MaitermTask` + `effectiveStatus`, `GET/POST /tasks`, `GET /overlord`, WS `overlord`, the action routes |
| `0.6` | adds `Chat.windowLabel` / `ChatDetail.windowLabel`, and `rules` + `agentTabIds` on the snapshot |
| `0.7` | adds `ChatDetail.subagents` and the WS `subagents` event (§4.3 `Subagent`) |
| `0.8` | adds `tool` + `detail` on `Chat`, `ChatDetail` and every `chat_state` frame |
| `0.9` | adds a **seventh lane, `dropped`**, to `status` / `effectiveStatus` everywhere a task is served or accepted; adds `MaitermTask.notes`; **removes `MaitermTask.topicId`** |
| `0.10` | adds `account` on `Chat`, `ChatDetail` and `chat_state` (§14); `GET /accounts`, WS `accounts`, `POST /accounts/active`; `GET /models?tab=`; attention kind `account` |

**0.9 is the one lane addition a client cannot treat as optional.** `dropped` is retracted work —
filed by mistake, superseded, decided against — and it arrives on rows the phone already renders,
so a board that switches exhaustively over six lanes gets an unhandled value rather than a missing
field. Three properties matter for rendering it: it is NOT a finish (folding it into Done makes a
completion count include work nobody did), it does NOT satisfy a dependent (a task blocked on a
dropped one stays blocked and says so), and it is reversible (`POST /tasks/{id} {status:"todo"}`
puts it back in play, which is the phone's undo for an agent that retracted the wrong row).
A pre-0.9 desktop never sends it and rejects it with 400, so a client may offer the write
unconditionally and let the status code decide.

**Treat any field newer than the version you require as optional anyway.** The table is a floor,
not a promise that nothing else is missing — and on a client where a render throw is unrecoverable,
optional-with-a-fallback costs less than being right.

## 14. v0.10 — Which account a chat is running as (shipped desktop-side 2026-09-22)

The desktop can hold several Claude logins (`docs/login.md`). One account per runtime is
*active*, a tab reads it **when its shell spawns**, and keeps it until it respawns — so two chats
can genuinely be billed to different accounts at once, and a chat keeps its account after the
active one changes. This section puts that on the wire. Reviewed with the maiLink client before
implementation; the four rules below are the review's.

### 14.1 The rules this shape exists to keep

1. **Absent ≠ `null` ≠ unknown.** `account` absent = a desktop older than 0.10. `null` = a tab
   the desktop KNOWS runs unmanaged (the feature is off, or no account was active at spawn).
   Unknown is its own explicit state, never `null` and never guessed.
2. **Never fill a tab's account in from the active account.** That is exactly the value the
   feature makes wrong: a tab keeps its spawn account after a switch. The desktop records the
   pairing at spawn (`pty::spawn_pty`, the single spawn path — reload, restore, resume and
   auto-resume all pass through it and each records afresh). A tab with a live PTY and no record
   answers `known: false`.
3. **No state claims a remote account is working.** The desktop can see that it *delivered* a
   token to an SSH session, never that the host accepted it — a token carries no identity to read
   back (login.md §2.4), and a wrong or expired one does not fail, it falls through to the host's
   own login (§6.1). So the vocabulary has no "applied".
4. **Failure is chat state, not a queued notification.** "Running as the wrong account" is a
   property of the tab now; it renders on the row and clears itself when the tab respawns
   correctly. The doorbell rings once, on the transition into it.

### 14.2 Shapes

```ts
type ChatAccount =
  | { known: false }        // a live tab with no spawn record, or an SSH session that never went
                            //   through the handoff. Render "account unknown" — never the active
                            //   account.
  | { known: true; remote: 'host_login' }
                            // SSH only: this host is not covered by the account (no token, or not
                            //   in its host list), so the agent runs as whatever the host is
                            //   signed in to. The desktop does not know who that is, so there is
                            //   no identity here and no `stale`. Informational, not an error.
  | { known: true;
      id: string;           // managed account id (matches AccountsSnapshot.accounts[].id)
      label: string;        // what the desktop shows — usually the email. On an SSH tab this is
                            //   the account the desktop INTENDED the remote to run as (the active
                            //   one at connect), never a claim that it does — `remote` says how
                            //   far it got. "Removed account" if the row is gone since spawn.
      org?: string;
      plan?: string;        // 'max' | 'pro' | 'team' | 'enterprise' | other, as reported
      stale?: true;         // this is not the CURRENTLY active account for its runtime (the tab
                            //   predates a switch, or management is now off). Pre-resolved, and
                            //   PUSHED: a switch sends chat_state for every tab it flips (§14.3).
      remote?: 'sent_unverified' | 'not_applied';
                            // SSH tabs only; absent on a local tab.
                            //   sent_unverified — the token was placed for this session. NOT proof
                            //     the host runs as `label`: offer nothing that reads as a check
                            //     mark. `/status` in the remote agent ("Auth token:
                            //     CLAUDE_CODE_OAUTH_TOKEN") is the human's check.
                            //   not_applied — the host IS NOT running as `label`: delivery failed,
                            //     the token expired, or it was prepared and never used. A warning.
      reason?: string;      // with not_applied: one human sentence. Never token material. LAN
                            //   only — never in a push (§14.5).
    };

// Chat and ChatDetail, and every chat_state frame whose account OR stale changed:
account?: ChatAccount | null;   // absent = pre-0.10 desktop; null = known unmanaged
```

**Which tabs count as SSH.** A tab whose PTY's foreground process is `ssh`/`mosh`, or that holds a
bridge tunnel — not the tunnel alone, which a typed `ssh` with the bridge off never gets and would
then be served the LOCAL shell's account. An SSH tab that never went through the handoff is
`{ known: false }`.

**A remote record belongs to one ssh process, not to the tab** — bound on evidence, on an edge
the desktop always sees, and never when a phone happens to look. One shell can run many ssh
sessions and only some go through the handoff (`ssh -t host claude` does not). Four review rounds
each found a rule that let a later ssh inherit an earlier handoff — a time window twice, a
per-tab file name, and binding lazily on a maiLink tick (an ssh that came and went unobserved was
then claimed by its own Up+Enter re-run). So:
- every handoff has its own name (tab id + a nonce per push), carried in the file name;
- the ssh maiTerm *types* (spawn, reconnect, auto-resume replay) carries that name in its argv,
  and the path that typed it binds the record when its own "ssh is up" poll sees an ssh whose
  command line names THAT handoff;
- where the fragment is typed *into* an ssh already running (the bridge's typed-ssh path, the
  manual "Inject maiTerm Env Vars"), the handoff binds to that process as it runs;
- a bound record is never re-bound, and serving a chat never binds anything.

An unbound record, or any ssh other than the bound one, is `{ known: false }`. On Windows the
binding probe does not exist yet, so every SSH chat there is `{ known: false }` — unknown, never
the local account.

**`null` needs nothing to be manageable.** A tab with an empty record answers `null` without a
process probe only while the feature is off; with it on, an empty-record tab over SSH is
`{ known: false }`.

**Claude chats only.** The handoff carries a Claude token. A Codex or Gemini chat over SSH is
`{ known: false }` and never rings the `account` doorbell.

### 14.3 Routes

| Route | Answer |
|---|---|
| `GET /accounts` | `AccountsSnapshot` |
| WS `accounts` | `{ type: "accounts", accounts: AccountsSnapshot, ts }`, full replace — sent on the first tick of every socket and on any change to the list, the active ids, or `enabled` |
| `POST /accounts/active` `{ runtime, accountId }` | §13.4 `{ accepted, confirmed, result?, reason? }` |
| `GET /models?tab=<tabId>` | `ModelOption[]` read from **that tab's** account (see 14.4). Without `tab`, the machine default as before |

```ts
interface AccountsSnapshot {
  enabled: boolean;         // false ⇒ feature off: say "not managed", not "no accounts"
  accounts: { id: string; runtime: string; label: string; org?: string; plan?: string;
              active: boolean }[];   // [] is a real answer
}
```

**`POST /accounts/active`** switches what *new* tabs launch as, and nothing else — it does not
respawn anything (a phone respawning tabs it cannot see kills agents mid-turn; a per-tab respawn,
if ever, is its own verb). `result` names the consequence so the phone can say it:

```ts
result: { tabsStillOnPrevious: number }   // live tabs whose account is now `stale: true`
```

**Every switch — phone or desktop — sends `chat_state` for each tab whose `stale` flipped**, carrying
`account`. The phone has no predicate to recompute `stale` with, deliberately, so a switch that
didn't push would leave it wrong in exactly the moments after the switch.

Refusals (`accepted: false, confirmed: true`): `reason: 'disabled'` (feature off), `'unknown_account'`,
`'runtime_mismatch'`, `'not_saved'` (the state file refused the write). **Run in Rust, not through the
webview** — it is a preference write, which Rust performs directly (as MCP `setPreference` does) and
broadcasts to every window. So `confirmed` is always `true`: a sleeping desktop screen cannot leave a
switch "sent, not confirmed". The desktop's own switch dialog (with its reload offer) does not appear
for a phone-initiated switch.

### 14.4 Why `/models` changed

`GET /models` read `~/.claude.json`'s `additionalModelOptionsCache` for every tab. Under a managed
account that cache belongs to whichever login owns the home file, so a chat running as account B
was offered account A's models — including pinned `source: 'account'` rows B may not be entitled
to. With `?tab=`, a managed local tab reads its own account root's `.claude.json`, and a known-unmanaged
tab reads the home file (unchanged behaviour — there the home file IS the tab's login).
**An unknown tab and every SSH tab get the builtin rows only** — the curated aliases expected on
every account, and never an empty array (`[]` reads as a desktop that predates `/models`). Pinned
`source: 'account'` rows appear only when the desktop knows whose cache they came from; guessing
them is rule 2's mistake in another field. An SSH tab's real list lives on the remote host and is
not fetched.

### 14.5 Doorbell

A new `AttentionKind`: **`'account'`**, sent once when a chat's `account.remote` *becomes*
`not_applied`. **Content-light like every push:** title = tab name, body = the fixed string
"Agent account not applied". Never `reason` (free text that can name hosts) and never `label`
(usually an email) — the push crosses a public relay onto a lock screen. The phone needs only
`tabId` + `kind`; the live reason is pulled over the LAN when the thread opens.

**What counts as a transition.** The previous state is in memory, so a restart would otherwise make
every restored SSH tab look newly failed and ring once each (the 08a1289 lesson). The rule:
observations in the first **three minutes after the desktop starts** are a baseline and never ring
— that window is session restore respawning every tab. After it, a tab entering `not_applied`
rings, including a brand-new or reloaded tab (a reload mints a new id, so "first observation per
tab" would have meant the doorbell almost never rang). The desktop's own "Agent account not
applied" notification still fires in the window — it is the user's local signal, not a push.

Older phones treat an unknown kind as urgent, which is correct for this one.
