# Mobile Packaging (iOS / Android) — Planning Sketch

> Status: **exploratory / for later consideration**. Not a committed plan.
> Date: 2026-05-28. Author: planning notes.

## TL;DR

- **Tauri 2 gives us the mobile build pipeline almost for free** — one codebase
  (Svelte 5 + xterm.js + CodeMirror frontend, `alacritty_terminal` Rust core) can
  target macOS, Windows, Linux, **iOS, and Android** from the same repo.
- **The wall is the backend process model.** Everything that makes maiTerm a
  *terminal* spawns local child processes (`portable-pty` → `/bin/bash`; even
  "SSH" shells out to the system `ssh` binary). Mobile sandboxes restrict or
  forbid that.
- **Clearest path: redefine the mobile product as a *remote* terminal** (an
  embedded SSH/Mosh client), not a local shell. One codebase, ~80% reuse, a small
  `#[cfg]`-gated transport layer swaps PTY (desktop) for SSH (mobile).
- **Cross-platform source: yes** (one codebase → 5 targets).
  **Cross-platform deployment: no** (each target ships its own bundle; mobile
  rides the App Store / Play Store, not our self-hosted updater).

---

## 1. The one constraint that determines everything

The frontend and the terminal *rendering* brain are already portable. The blocker
is that the whole terminal *model* is local-process-based:

| Concern | Where | Mobile problem |
|---|---|---|
| Spawn shell | `pty/manager.rs` → `native_pty_system().openpty()` + `spawn_command` | iOS forbids `fork`/`exec`; Android restricts it |
| "SSH" sessions | detection of user-typed `ssh`/`mosh` in a local PTY (`is_ssh_command`) | relies on a local PTY + system `ssh` binary |
| MCP reverse tunnel | `commands/ssh_tunnel.rs:68` → `tokio::process::Command::new("ssh")` | shells out to system `ssh`; no such binary on iOS |

**Key insight:** maiTerm does **not** contain an SSH *implementation* today — it
detects and wraps the OS `ssh` binary. None of that transfers to mobile. Mobile
needs a real in-process SSH client.

## 2. Platform reality

| | Can spawn local processes? | Verdict |
|---|---|---|
| **iOS** | **No.** Sandbox forbids `fork`/`exec`. No `/bin/sh`, no `/usr/bin/ssh`. | Local terminal **impossible**. Only a network terminal (in-process SSH/Mosh) works. Same reason Blink Shell / Termius / Prompt are SSH clients, never local shells. (iSH is the exception — ships a usermode x86 emulator; huge effort, shaky App Store footing.) |
| **Android** | **Partial.** Can exec, but since API 29 (W^X) only from the app's native-lib dir, not writable storage. | Local shell *possible* but heavy (Termux-style binary bootstrap) and Play Store distribution is hostile to it. In-process SSH works cleanly. |

A straight lift-and-shift = beautiful UI with a **dead terminal** on iOS.

## 3. Recommended path — Path A: SSH-first remote client

The unlocking decision is **product, not technical**: mobile maiTerm = SSH client,
not local shell. Then one codebase serves all platforms.

### Shared vs. platform-conditional

```
┌─────────────────────────────────────────────┐
│  SHARED (one codebase — ~80%)                │
│  • Svelte 5 / xterm.js / CodeMirror UI       │
│  • alacritty_terminal core, render.rs,       │
│    serialize, search, OSC, triggers, themes, │
│    notes, workspaces, state/persistence      │
├─────────────────────────────────────────────┤
│  PLATFORM-CONDITIONAL (~20%, via #[cfg])     │
│  desktop → PtyChannel (portable-pty)         │
│  mobile  → SshChannel (russh)                │
│  desktop-only plugins: updater, window-state │
└─────────────────────────────────────────────┘
```

**Reuse unchanged:** entire render pipeline (`alacritty_terminal` core →
`render.rs` ANSI viewport → xterm.js), scrollback serialize/restore, search, OSC
handling, notes, workspaces, triggers, themes, the whole Svelte UI.

**Replace:** the byte source. Today a PTY reader thread feeds bytes into the
terminal core. On mobile, feed it from an **embedded pure-Rust SSH client**
([`russh`](https://crates.io/crates/russh), formerly thrussh; or
`async-ssh2-tokio`). These compile for iOS/Android targets; the system `ssh`
binary does not exist there.

### The transport refactor (the core architectural move)

```rust
trait ByteChannel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()>;
}

#[cfg(desktop)] struct PtyChannel { /* portable-pty */ }
#[cfg(any(mobile, feature = "ssh-transport"))] struct SshChannel { /* russh */ }
```

Everything downstream of the byte stream (`handle.rs`, `render.rs`, `serialize`,
`search`, `osc`) stays identical — it already consumes a byte stream, not a PTY
specifically.

> Note: optionally compile `SshChannel` on desktop too (behind a feature flag) —
> it would give the desktop app a *native* SSH client instead of wrapping the OS
> `ssh` binary, which is a nice bonus and a good way to test the mobile transport
> on desktop first.

### Why this is also the better *product*

maiTerm already has the Claude Code hooks integration, multi-session awareness
(`getClaudeSessions`), and notifications. The SSH-client framing turns mobile into
a **remote command center for your agents** — watch Claude sessions running on
dev boxes from your phone, get Stop-hook notifications, jump in to approve a
permission prompt. Differentiated, not a degraded desktop clone.

## 4. Implementation steps

1. **Toolchain spike (½–1 day):** `cd src-tauri && cargo tauri android init` +
   `cargo tauri ios init` → generates `gen/android` and `gen/apple`. Needs
   Android Studio/NDK + JDK; Xcode + a $99/yr Apple Developer account for iOS.
   Confirm the UI renders in a simulator. (`lib.rs:43` already has
   `#[cfg_attr(mobile, tauri::mobile_entry_point)]`, so the scaffold is partly
   mobile-aware.)
2. **Make the backend compile for mobile (the real grind):** `#[cfg(desktop)]`-gate
   everything mobile can't link:
   - `portable-pty` (the PTY manager spawn path)
   - `tauri-plugin-updater` (mobile updates via stores)
   - `tauri-plugin-window-state` (no windows on mobile)
   - `tauri-plugin-process`
   - the `notify` crate (its `macos_kqueue` feature is desktop-only)
   - the dev-only `axum` / `tokio-tungstenite` MCP-bridge + SSH-tunnel servers
     (already feature-gated — keep them out of mobile)
   - `tauri` `macos-private-api` feature (macOS-only)
3. **Introduce `ByteChannel`** and wire `SshChannel` (russh) as the mobile transport.
4. **Connection + key management UI:** host/user/auth, known-hosts, keys in the
   platform secure store (iOS Keychain / Android Keystore).
5. **Mobile UX:**
   - touch accessory key-row (Esc / Tab / Ctrl / arrows / `|` `/` `-`) above the
     soft keyboard — non-negotiable for xterm.js on touch
   - keyboard insets / safe-area handling
   - reconnect-on-resume (mobile kills sockets on backgrounding; Mosh-style
     resilience is the gold standard but a bigger lift than plain SSH)
   - the desktop splits/window model collapses to a mobile tab/nav UI
6. **Storage:** `rusqlite` (bundled) works on both; write to Tauri's sandboxed
   app-data path (already abstracted via the path API).

## 5. Build & release setup

### Cross-platform *source* — yes. Cross-platform *deployment* — no.

One codebase compiles to five targets, but there's no single artifact that runs
everywhere. Each target ships its own bundle on its own channel:

| Target | Artifact | Distribution | Updates |
|---|---|---|---|
| macOS | `.dmg` / `.app` | Direct download (current flow) | `tauri-plugin-updater` + `latest.json` |
| Windows | `.exe` | Direct download | updater |
| Linux | `.deb` / `.AppImage` | Direct download | updater |
| **iOS** | `.ipa` | **App Store only** | **Store-mediated** (no Tauri updater) |
| **Android** | `.apk` / `.aab` | Play Store / sideload | Store-mediated |

**Implication:** the existing desktop Release workflow (`latest.json` + GitHub
Release machinery) **stays as-is for the three desktop targets**. Mobile gets a
*parallel* lane:

- **Build:** `cargo tauri ios build` / `cargo tauri android build`
- **iOS:** signed `.ipa` → App Store Connect (TestFlight for beta, then review)
- **Android:** signed `.aab` → Play Console (internal testing track → production)
- Updates flow through the stores, **not** our self-hosted updater — so
  `tauri-plugin-updater` and `latest.json` are desktop-only.

### CI lane (later)

- macOS runner can build iOS (needs Xcode + provisioning profiles + signing certs
  in CI secrets).
- Android can build on Linux/macOS runners (NDK + signing keystore in secrets).
- Keep the mobile jobs separate from the existing desktop matrix — different
  signing, different upload destinations, different cadence (store review latency).

## 6. Effort & risks

**Effort (Path A):**
- Toolchain bring-up: days.
- Transport refactor + cfg-gating: ~couple of weeks to a usable alpha.
- Long tail: mobile UX (keyboard bar, reconnect, secure key storage) + store
  paperwork/review.

**Risks / gotchas:**
- Code that calls `native_pty_system()` won't even **link** on iOS — the cfg-split
  is mandatory, not cosmetic.
- **App Store:** a generic "connect to your server over SSH" app is fine
  (Termius/Blink exist); anything that looks like it downloads + runs code
  *locally* is a rejection risk on iOS.
- WebView differs per platform (WKWebView iOS/macOS, Android System WebView,
  WebView2 Windows) — occasional CSS/JS quirks. The dev-CSP/`unsafe-eval` lessons
  from the MCP bridge apply to WKWebView.
- Mobile network sockets die on backgrounding — reconnect UX is essential.
- iOS requires the $99/yr Apple Developer account and review cycles.

## 7. Alternatives considered

- **Path B — Android-local-shell + SSH-everywhere:** ship a Termux-style binary
  bootstrap for a real local shell on Android; iOS stays SSH-only. → two
  divergent products, much more packaging, Play Store friction. Only if a local
  Android shell is a hard requirement.
- **Path C — lift-and-shift as-is:** `tauri ios/android init` and build. Worth
  ~one day as a spike to prove the toolchain + see the UI render, but **not
  shippable** (dead terminal on iOS).
- **Flutter / React Native rewrite:** true cross-platform, but throws away the
  `alacritty_terminal` core and the entire Svelte/xterm frontend. No leverage.
- **Shared Rust core + native Swift/Kotlin UIs:** most native feel, but now three
  UIs to maintain. Opposite of cross-platform consolidation.

→ **Tauri (Path A) maximizes reuse of what's already built.**

## 8. Open questions / decisions for later

- [ ] Is the mobile product **SSH-only** (recommended), or do we want a local
      Android shell too (Path B)?
- [ ] Both iOS **and** Android, or start with one? (Android is lower friction to
      ship; iOS is the harder constraint and the better demo.)
- [ ] Mosh support for connection resilience, or plain SSH + manual reconnect for v1?
- [ ] Should desktop also adopt the native `russh` transport (replacing the
      OS-`ssh`-binary wrapping), or keep that mobile-only?
- [ ] Key storage: roll our own Keychain/Keystore bridge, or use an existing
      Tauri plugin?
- [ ] How much of the desktop UI (splits, workspaces sidebar) maps to mobile vs.
      gets a bespoke mobile layout?

## 9. Suggested first concrete step

Run the **Path C spike** (1 day): `cargo tauri ios init` + `android init`, build,
and catalog exactly what fails to compile. That converts the cfg-gating work in
§4.2 from an estimate into a concrete checklist, with zero commitment.
