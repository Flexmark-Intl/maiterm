# Split-Pane Rendering Performance — Analysis & Improvement Ideas

> Status: **analysis / proposed**. Not a committed plan. Date: 2026-06-28.
> Owner: Darryl. Scope: why split panes degrade rendering performance while
> separate windows don't, and the realistic options for closing the gap.

## TL;DR

- **The Rust backend is already multi-threaded** — each PTY renders on its own
  thread, across cores. The bottleneck is **not** a lack of threads in our code.
- **The bottleneck is WebKit's one-JS-main-thread-per-WebContent-process model.**
  In this stack the unit of parallelism is the *webview/process*, not the thread.
- **Multiple windows are fast because each window is its own WebContent process**
  (separate main thread + GPU context), so the OS spreads the load across cores.
  **Split panes are slow because every visible pane shares one webview** — one
  main thread, one event loop, one GPU context — and the render work serializes.
- You **cannot** cleanly make a single webview's DOM rendering multi-threaded:
  Web Workers can't touch the DOM, and the OffscreenCanvas route both reintroduces
  the renderer ghosting we deliberately eliminated and hits WebKit's weak
  worker-WebGL support.
- **Two viable directions:** (A) *pragmatic* — trim redundant main-thread work so
  the single thread keeps up (visibility-gating + catch-up-on-reveal, frame
  coalescing); (B) *architectural* — give each pane its own webview (true
  parallelism via Tauri's `unstable` multi-webview feature, at the cost of one
  WebContent process per pane).
- **Recommendation:** do (A) first and measure; only reach for (B) if real
  parallelism is still needed afterward.

---

## 1. The observed problem

Several split panes visible at once in one window noticeably degrade rendering
responsiveness. The *same* number of terminals spread across separate windows does
not. The instinct is "split panes need to be multi-threaded" — but the work is
already threaded where it can be; the constraint is elsewhere.

## 2. Root cause: where the single thread is

There are two sides to the pipeline.

**Rust backend — already parallel.** Each PTY has its own reader, writer, and
emitter threads. The emitter coalesces grid-dirty notifications to one render per
`FRAME_INTERVAL` (16ms, ~60fps), renders the viewport to ANSI, and emits
`term-frame-{ptyId}` app-wide — see `src-tauri/src/pty/manager.rs:19` and the
emitter thread at `manager.rs:366-407`. N busy PTYs = N independent emitter threads
on N cores. This side scales fine.

**Frontend webview — single-threaded, and shared by all panes in a window.** WebKit
gives each WebContent process exactly one JS main thread. Everything downstream of a
frame lands there:

- Tauri event delivery + payload deserialization
- `terminal.write()` → xterm.js ANSI parsing
- xterm.js **DOM renderer** mutating the DOM (style recalc on the main thread)
- Svelte reactivity / `$effect`

With separate windows, each window is a separate WebContent process, so each gets
its own main thread that the OS can schedule on a separate core (see
`WebviewWindowBuilder::new(...)` per window at
`src-tauri/src/commands/window.rs:386`). With split panes, **all** visible panes
portal into **one** document in **one** webview, so all of that work serializes onto
one thread.

| | Split panes (1 window) | Multiple windows |
|---|---|---|
| Renderer processes | 1 | N (one per window) |
| JS main threads | 1 (shared) | N (OS spreads across cores) |
| Frame streams into event loop | all of them | only that window's |
| GPU context | 1, shared | 1 per window |

Same total work, very different scheduling. **Adding a split adds terminals but not
threads to render them.**

### Three maiTerm-specific amplifiers

1. **Every visible split pane runs its own live xterm.js DOM renderer at once.**
   Unlike background *tabs* (only the active one shows), split *panes* are all
   visible simultaneously, so each is actively diffing the DOM on the shared thread.
2. **Rust emits a full-viewport repaint per frame, not a delta.** `render.rs:53`
   prefixes each frame with `\x1b[H\x1b[2J` (home + clear) then the full content, so
   every `terminal.write()` is non-trivial work — multiplied by the number of
   streaming panes.
3. **The frame listener is not gated on visibility.** `TerminalPane.svelte:452-477`
   registers the `term-frame` listener once in `onMount` and always calls
   `terminal.write()`, even for a tab stacked *behind* the active one inside a split
   pane. So a multi-tab split pane also pays for its hidden tabs. (The canvas/WebGL
   renderer *is* gated on `visible` at `TerminalPane.svelte:1224-1247`; DOM is the
   default, so that's usually not the dominant cost.)

## 3. Why "just make it multi-threaded" doesn't work cleanly

The frontend bottleneck is the one thing you *can't* trivially thread:

- **Web Workers** are real threads but have **no DOM access**. The xterm DOM
  renderer lives and dies on the main thread; it can't move into a worker.
- **OffscreenCanvas in a worker** is the only off-main-thread render path in a
  browser, but it forces a canvas/WebGL renderer — exactly what we abandoned. The
  "DOM is default" decision exists because WebGL/Canvas **ghost** under our
  full-frame 60fps streaming (see `src/lib/components/terminal/CLAUDE.md`). This
  path reintroduces the ghosting **and** WebKit/WKWebView's worker-WebGL support is
  historically weak. Double penalty.
- **Render in Rust, blit native surfaces over the webview** (wgpu per pane) is real
  parallelism but means a full GPU terminal renderer plus per-pane native overlay
  surfaces — z-order, input routing, layout sync, platform-specific NSView
  plumbing. Powerful, very much not clean.

**Takeaway:** in this stack you don't parallelize a webview's rendering — you add
webviews. The unit of parallelism is the process, not the thread.

---

## 4. Approach A (pragmatic): trim redundant main-thread work

Don't add threads — make the one thread do less. Lower risk, no architecture
change, and often closes most of the gap because much of the "splits are slow" cost
is redundant work.

### A1. Visibility-gate frame writes + catch-up on reveal

Skip `terminal.write()` for tabs stacked behind the active one in a split pane (and,
optionally, throttle panes that are off-screen). **This must be paired with a
catch-up repaint on reveal — gating alone is a correctness bug.**

**Why the catch-up is mandatory.** Frames are only emitted when the Rust grid is
`dirty`. A background tab that finished its work and is now idle at a prompt produces
**no more frames**. So if you skip writes while hidden and do nothing on reveal,
xterm keeps painting the last pre-hide frame and **nothing ever arrives to correct
it** — stale forever, not just "until the next update."

**Why the catch-up is clean.** Rust (alacritty_terminal) is the source of truth;
xterm has `scrollback: 0` and is just a paint surface. So you can ask Rust for the
*current* viewport at any time and get the true present state — one deterministic
repaint straight to "now," not a jump from old content to slightly-less-old content.

**The primitive already exists.** The scroll-hold path already pulls a fresh
authoritative frame on demand and writes it:

```js
// TerminalPane.svelte:464 — fresh authoritative frame on demand
scrollTerminalTo(ptyId, userScrollOffset).then(held => {
  terminal.write(new Uint8Array(held.ansi));
});
```

Catch-up-on-reveal is the same call at offset 0.

**Seamless vs lazy reveal:**

- *Lazy:* flip the tab visible, then async-request the frame → a few-ms window where
  the stale frame shows before catch-up lands (perceptible for a tab that was
  streaming heavily).
- *Seamless (preferred):* request the frame **while still hidden**, write it, then
  reveal → no flash; the switch just "waits" one frame (imperceptible, and cheaper
  than the fit/portal work a switch already does).

**Why this is less scary than it sounds:** the flash only exists for tabs that were
*actively changing while hidden*. For a tab that went idle in the background — the
common case — the last frame written before gating **is** its final state, so there's
nothing stale to correct. No flash, no catch-up needed.

### A2. Coalesce harder when many panes stream at once

When N panes stream simultaneously, the event loop drains N independent ~60fps
streams in lockstep. Batch or drop intermediate frames (keep only the latest pending
frame per pane per animation frame) so the main thread isn't doing N full repaints
every 16ms. This complements A1: gating removes hidden work, coalescing smooths the
visible work.

**Net cost of Approach A:** the main thread stops servicing background/redundant
streams; the only added cost is one Rust round-trip per tab switch (cheaper than the
existing switch work). No architecture change, no ghosting risk.

## 5. Approach B (architectural): one webview per pane

For genuine parallelism, give each split pane its **own** child webview. Tauri 2
supports multiple webviews per window behind the `unstable` Cargo feature
(`window.add_child(WebviewBuilder, position, size)`). Each webview is its own
WebContent process — exactly like a separate window, just laid out within one window
frame. This reuses the mechanism that already makes multiple windows fast, and keeps
the DOM renderer (no ghosting regression).

**Costs / risks:**

- Tauri **unstable** feature — rough edges around focus, input, resize, z-order,
  transparency.
- Each pane becomes a full WebContent process (tens of MB) — the same cost as the
  multiple windows we already accept, but now per pane.
- The **portal pattern changes substantially**: terminals would attach into
  per-webview documents, and event/IPC wiring becomes per-webview. Non-trivial
  surgery.

**Rejected alternatives** (see §3): Web Workers (no DOM), OffscreenCanvas
(ghosting + weak WebKit support), native wgpu overlays (large, platform-specific).

## 6. Recommendation & sequencing

1. **Approach A first.** Implement A1 (visibility-gating + seamless catch-up) — a
   small, safe, high-leverage change — then A2 (coalescing). Measure the improvement
   with several busy panes.
2. **Only if still needed, Approach B.** Multi-webview is the architecturally
   correct way to get true threading here, but it's a big bet on a Tauri unstable
   feature for a fairly niche win. Given the tiny user base and that splits-degrade
   is a comfort issue rather than a correctness bug, it should clear a high bar.

## 7. Code reference appendix

- `src-tauri/src/pty/manager.rs:19` — `FRAME_INTERVAL = 16ms` (~60fps)
- `src-tauri/src/pty/manager.rs:366-407` — per-PTY emitter thread; condvar park,
  coalesce-to-60fps, emit `term-frame-{ptyId}`
- `src-tauri/src/terminal/render.rs:53` — `\x1b[H\x1b[2J` full-viewport repaint per
  frame
- `src-tauri/src/commands/window.rs:386` — `WebviewWindowBuilder::new(...)`, one
  webview (WebContent process) per window
- `src/lib/components/terminal/TerminalPane.svelte:452-477` — `term-frame` listener,
  **unconditional** `terminal.write()` (gating target for A1)
- `src/lib/components/terminal/TerminalPane.svelte:464` — on-demand authoritative
  frame fetch (the catch-up primitive)
- `src/lib/components/terminal/TerminalPane.svelte:1224-1247` — canvas renderer
  gated on `visible`
- `src/lib/components/terminal/CLAUDE.md` — why DOM is the default renderer (ghosting
  under full-frame streaming)
