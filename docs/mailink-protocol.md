# maiLink — Mobile Companion Protocol & Architecture

> Status: **design / contract draft v0.3**. This is the shared contract between the
> maiTerm **desktop** side (this repo) and the **maiLink mobile app** (separate codebase,
> built collaboratively with the maiLink agent). Date: 2026-06-30.
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
    pub last_seen_at: i64,
}
```

Revocation = remove the record; its bearer token stops validating immediately.

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
| `GET  /chats/{tabId}?before={msg_id}&limit=N` | One chat + transcript (paging params reserved) | → `ChatDetail` |
| `GET  /chats/{tabId}/context?lines=N` | Distilled plain-text context | → `{text, truncated}` |
| `POST /chats/{tabId}/message` | Send a message / proactive command | `{text, submit?:true}` → `{status:"queued"\|"delivered", msg_id}` |
| `POST /chats/{tabId}/respond` | Answer a pending permission/question | `{choice, prompt_id}` (see §5) → `{ok}` \| `{ok:false, reason:"stale"}` |
| `POST /chats/{tabId}/activate` | Activate/focus/resume a designated tab | `{}` → `{state}` |
| `POST /chats/{tabId}/interrupt` | Send Esc (stop the agent) | `{}` → `{ok}` |
| `GET  /heartbeat` | Liveness + server clock | → `{ok, now, server_name}` |

### 4.2 WebSocket (live chat channel) — `GET /mailink/v1/ws` (upgrade)

Bidirectional, opened while the app is foreground. Server→client events:

```jsonc
{ "type": "chat_state", "tabId": "...", "state": "active|idle|permission",
  "runtime": "claude", "tool": "Bash", "detail": "rm -rf ./dist", "ts": 0 }
{ "type": "message", "tabId": "...", "role": "agent|user|system",
  "text": "...", "msg_id": "...", "ts": 0 }          // a new transcript turn
                                                     // for a user echo, msg_id === the id POST /message returned
{ "type": "attention", "tabId": "...", "kind": "permission|idle_done|question",
  "summary": "Needs permission: Run rm -rf ./dist",
  "prompt": { "prompt_id": "p_7f3a", "kind": "permission", "text": "Run: rm -rf ./dist",
              "options": ["Yes","Yes, don't ask again","No"] }, "ts": 0 }
                                                     // `prompt` mirrors pendingPrompt; present for permission/question,
                                                     // omitted for idle_done. Lets the app render decision buttons on the
                                                     // live path with no follow-up GET. GET /chats/{tabId} stays source of truth.
{ "type": "chats_changed" }                           // roster/designation changed; re-GET /chats
```

Client→server frames are optional conveniences mirroring the REST actions (`message`,
`respond`, `activate`, `interrupt`) so the foreground app can avoid REST round-trips; both
paths converge on the same backend handlers.

Presence: while ≥1 device holds a live WS for a tab, that tab is "covered" and the doorbell
is **suppressed** (no redundant push). On WS close, coverage drops and future attention
events doorbell again.

### 4.3 Shapes

```typescript
interface Chat {
  tabId: string;
  title: string;            // tab name
  workspace: string;        // workspace name (grouping)
  runtime: 'claude' | 'codex' | 'gemini';
  state: 'active' | 'idle' | 'permission' | 'dormant';
  unread: boolean;          // idle/attention not yet seen on a device
  lastActivityTs: number;
  preview: string;          // last line(s) of distilled context
}
interface ChatDetail extends Chat {
  transcript: Message[];    // distilled turns, newest last
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
- **Interrupt**: inject `\x1b` (Esc) — the documented "human interrupts the agent" gesture.

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
  `title` + `kind` (`permission`/`idle_done`), which is all the alert renders. Apple and the
  relay learn *that* an agent wants you and which tab, never the prompt. The phone wakes, opens
  the WS over LAN/WireGuard, and pulls the real content.
- `tab_id` drives `apns-collapse-id`/`thread-id` so repeated pings for one tab coalesce.
- `apns-priority: 10` + a time-sensitive alert for permission/question; an `active` alert for
  done/idle. Respect the phone's own mute.

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
  model?: string;         // normalized display name: "Opus 4.8", "GPT-5-codex", "Gemini 2.5 Pro"
  effort?: string;        // runtime effort tier; OMITTED when the runtime has none, or not sourceable
                          //   (Claude effort lives only in the statusLine payload maiTerm doesn't receive → omitted today)
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
  pendingPrompt?: PendingPrompt;
}

export interface Turn {
  msg_id: string;
  thread_id: string;
  author?: Participant;         // absent => the human/user
  role: 'agent' | 'user' | 'tool' | 'system';
  kind?: 'terminal_snapshot';   // present ONLY on the raw-scrape fallback (see below); absent => distilled turn
  text: string;                 // source markdown (for kind:"terminal_snapshot", raw newline-delimited grid text, NOT markdown)
  ts: number;
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
    model-dependent limit (1,000,000 for `[1m]`/`-1m` model ids, else 200,000). `model`
    normalized from `message.model` ("claude-opus-4-8" → "Opus 4.8").
  - *Codex*: the rollout's last `token_count` — `last_token_usage.total_tokens` over the
    stated `model_context_window` — and `turn_context.model` ("gpt-5.5" → "GPT-5.5").
  Emitted on the `/chats` object, in `chat_detail`, and on the WS `chat_state` event (so the
  gauge steps live per turn). `effort` is omitted (only in Claude Code's statusLine payload,
  not received). Gemini tabs get no `meta` (no transcript source yet).
