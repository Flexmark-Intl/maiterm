# Browser link — an agent diagnoses and drives the Chrome tabs its human linked (2026-10-20)

A maiTerm agent tab can be linked to tabs in the human's own Chrome (or Brave, Edge, Arc — any
Chromium browser), and the agent then diagnoses and drives them over MCP: its console
errors and network requests, the page itself, and clicking, typing and navigating in it. It is the human's real browser with its real logins, not a separate
automation profile, which is why the extension route was chosen over `--remote-debugging-port`
(Chrome refuses remote debugging on the default profile, so that route gets an empty one).

```
agent ─MCP─▶ maiTerm (Rust, browser_link/) ─WebSocket 127.0.0.1─▶ extension ─chrome.debugger─▶ the linked tab
```

The extension lives in this repo (`chrome-extension/`, plain MV3 JavaScript, no build step) and
is versioned on its own (`chrome-extension/manifest.json`, tags `ext-v*`).

## 1. Rules

1. **Only the human links.** A link is made by a click in the extension's popup, on the Chrome
   tab being linked. No MCP tool creates, moves or widens a link. An agent that has no link is
   told to ask its human for one. The one exception is a tab a linked page OPENS (§1.6), which
   comes from a page the human already handed over.
2. **An agent drives only its own links.** Every browser tool acts on the links of the CALLING
   tab, from its stated identity (`OWN_TAB_ONLY_TOOLS` + `PEER_ADDRESSING_TOOLS`): there is no
   `chromeTabId` argument, an explicit `tabId` naming another tab is refused, and `tab` is a
   handle among the caller's OWN links.
3. **A Chrome tab has one agent; an agent may hold several** (`MAX_LINKS_PER_TAB` = 8). Each link
   gets a handle, `b1`, `b2`…, never reused within its maiTerm tab, so a stale handle fails
   instead of reaching another page. Linking a Chrome tab again moves it; the agent that lost it
   is told. A tool without `tab` acts on the agent's CURRENT tab (`Registry.current`): set by its
   first link the human makes (never a re-announced one) and by naming a tab, and by nothing else — a tab the human links beside it, or one
   that joins, must not become where an untargeted click lands. When the current tab goes and
   several remain (or maiTerm restarted), an untargeted call is refused until the agent names one.
   Refs carry their tab (`b1:e42`, written by the snapshot), so a ref always acts where it was read.
   Handles are never reused: the extension keeps each agent tab's highest handle
   (`handleFloors`, sent on `hello`), since a maiTerm restart empties the counter.
   What changed that the agent didn't do (a tab joined, closed, moved, was unlinked) is held and
   handed over as `linked_tabs_changed` on its next browser tool call.
4. **The human can always see and stop it.** Chrome's own "started debugging this browser" bar
   shows while a linked tab is attached — in EVERY tab of every window (Chrome's choice: it is
   browser-wide, no extension can scope or hide it; only Chrome's `--silent-debugger-extension-api`
   flag does). So every surface that mentions it says it is browser-wide and that only linked
   tabs are attached, or a human reads it as "it took all my tabs". Cancelling that bar detaches
   every session and so UNLINKS every linked tab (the human saying stop — re-attaching silently
   would overrule them). Closing a Chrome tab unlinks it. The extension stays attached for the
   life of a link, by decision (2026-10-22): idle-detach would hide the bar but lose the console
   and network record of a bug the human reproduces themselves.
5. **Links are in memory.** Chrome tab ids do not survive a browser restart, so nothing is
   persisted on a `Tab`. The extension keeps its links in `chrome.storage.session` (lives as long
   as the browser) and re-announces them on every (re)connect; maiTerm accepts a re-announced
   link only for a maiTerm tab that still exists. A maiTerm tab RELOAD carries its link to the new
   id (`browser_link::remap_tab`, called from `reloadTab`'s remaps). Every reload's old→new id is remembered (`Registry.renamed`), and both a re-announced
   link and the 5 s prune follow that chain, so a link that missed the remap (the extension was
   disconnected, or its `hello` was in flight) still reaches the reloaded tab.
6. **A tab a linked page opens joins that page's agent** (the extension,
   `webNavigation.onCreatedNavigationTarget`'s `sourceTabId` — NOT `tabs.onCreated`'s
   `openerTabId`, which names the tab in FRONT when a background tab opens one, i.e. exactly when
   an agent works in a tab the human isn't looking at): when it is a popup window (a sign-in window), or an action that can
   open a tab (click, type, press, navigate, eval — never a read) is running on the opener or
   finished under 2 s ago (a running one counts for at most 30 s: an eval may never settle). A tab the HUMAN opens from a linked page (a middle-click) stays theirs,
   even while the agent reads that page. maiTerm refuses an opener that isn't the same agent's link.
   An extension older than 0.2.0 (still running in a Chrome that hasn't reloaded it) keeps one tab
   per agent: maiTerm drops that agent's other links when it links one, as that extension does.
8. **A linked tab is marked in Chrome's own tab strip** (`chrome-extension/marker.js`), because
   Chrome's debugging bar shows in every tab and says nothing about which one is linked: each
   agent's linked tabs share a tab group per window, "maiTerm · <agent tab>", in a colour fixed
   per agent. Every way a link ends (unlink in the popup or maiTerm, Cancel on the bar, the
   feature off, the agent tab closed, a reconnect maiTerm doesn't accept) ungroups it and PUTS IT
   BACK after the tab it followed (else at its old index) — the human asked for exactly that.
   Moving a link between agents carries the original placement. Never touched: a tab already in
   the human's own group (not grouped), pinned or popup-window tabs (not grouped), and a tab the
   human has since dragged out of our group or to another window (ungrouped only, left where
   they put it). Chrome groups on its own as well (a tab opened from a grouped tab joins that
   group), so: a joined tab already in the agent's group is recorded as `joined` (ungrouped in
   place at the end, never moved); any tab in one of our groups that isn't linked is taken out
   after 600 ms (checked on `onCreated` too — a tab created inside a group reports no change);
   and at worker start a "maiTerm · " group no link owns is ungrouped, because
   `chrome.storage.session` (the links and their placements) is cleared by an extension reload or
   update and a browser restart while Chrome keeps the groups. Putting a tab back between two
   tabs still in the group would make Chrome regroup it, so it is then taken out to the group's
   edge. maiTerm's tab-strip mark and the popup's "Linked tabs" list bring a linked tab
   to the front (`reveal`, a command the extension runs without the debugger; no agent tool).
7. **The popup lists agent tabs most recently used first**, each window's front tab (active
   workspace → active pane → active tab) above all. Recency is in memory (`browser_link::touch_tab`,
   from `set_active_tab` / `set_active_pane` / `set_active_workspace` and window focus); after a
   restart the tree order stands in.

## 2. Discovery and authentication (native messaging)

The extension cannot read files or guess ports, and anything on the machine can open a localhost
socket, so the port and a secret reach it through Chrome **native messaging**:

- maiTerm writes a host manifest into each installed Chromium browser's `NativeMessagingHosts`
  folder (Windows: the registry key pointing at a manifest file). The manifest's
  `allowed_origins` names only our extension ids, and Chrome enforces that.
- The host program is **the maiTerm binary itself**: `main()` checks for a
  `chrome-extension://…` argument (Chrome passes the caller's origin) before anything else and
  runs `browser_link::native_host_main()` — read one framed message, answer with the discovery
  file, exit. No GUI is touched.
- The discovery file `browser-link.json` (app data dir, mode 0600) is written when the MCP server
  binds: `{ port, token, instance, pid }`. The token is fresh every launch.
- Host names: `com.maiterm.browser` (release) and `com.maiterm.browser_dev` (dev build). The
  extension asks both and holds one connection per instance that answers, so a dev maiTerm and an
  installed one can both be linked (the popup labels them).
- Manifests are (re)written at launch and when the human turns the feature on, so a moved app
  heals itself. Turning it off removes them.

Extension ids: `mdaccealpnngffgibmnijfgakddfgjpa` (unpacked — fixed by the public `key` in the
manifest). The Chrome Web Store id is added to `ALLOWED_EXTENSION_IDS` when the listing exists.

## 3. Wire protocol (extension ↔ maiTerm), `protocol: 1`

WebSocket `ws://127.0.0.1:<port>/browser-link`. The upgrade is refused unless `Origin` is
`chrome-extension://<allowed id>`. JSON text frames, JSON-RPC shaped:
`{id, method, params}` → `{id, result}` | `{id, error}`; notifications have no `id`.
Both sides send requests.

Extension → maiTerm:

| method | params | result |
|---|---|---|
| `hello` (first frame, within 5 s) | `{token, protocol, version, browser, links:[Link], handleFloors?:{maitermTabId: n}}` | `{instance, protocol, accepted:[chromeTabId], handles:{chromeTabId: handle}}` (handles beside `accepted`, not in it: an older extension reads `accepted` as ids) |
| `listTabs` | — | `{tabs:[{id, name, workspace, window, runtime, front, linked:[{handle, title, browser}]}], windows}` agent tabs only, in §1.7 order |
| `link` | `{maitermTabId, chromeTabId, title, url, openerChromeTabId?}` | `{ok, handle}` / error (cap, closed tab, opener not that agent's) |
| `unlink` | `{chromeTabId, reason?}` — `unlinked` \| `closed` \| `cancelled` \| `moved`, for the agent's notice | `{ok}` |
| `tabUpdated` (notif) | `{chromeTabId, title, url}` | |
| `ping` | — | `{}` (keeps the MV3 service worker alive) |

maiTerm → extension:

| method | params | result |
|---|---|---|
| `command` | `{chromeTabId, action, args}` | action-specific, or `{error}` |
| `unlinked` (notif) | `{chromeTabId, reason}` | maiTerm dropped a link: its tab closed (checked every 5 s while connected), the human unlinked it in maiTerm, or turned the feature off |
| `relinked` (notif) | `{chromeTabId, maitermTabId, maitermTabName}` | its maiTerm tab was reloaded under a new id; without this the next `hello` re-announces the old id and maiTerm refuses it as a closed tab |

Every change the extension makes to its stored links goes through one queue (`mutateLinks`): each
is read → await → write, and two interleaved lost the newer write (a re-link racing maiTerm's
`unlinked` for the old tab).

**Turning the feature off ends everything at once** (`browser_link::shut_down`): every link is
`unlinked`, every connection closed, a new `hello` is refused, and the tools refuse too — an
agent's tool list is cached, so hiding the tools alone left them working.

`Link = {maitermTabId, chromeTabId, handle, openedBy?, title, url}`. The toolbar badge shows a linked tab's handle.

A major-version mismatch in `protocol` is refused with a sentence the popup shows ("update the
extension" / "update maiTerm"). Additive changes don't bump it.

## 4. Actions and MCP tools

The extension does all CDP work (`chrome-extension/driver.js`); Rust relays and times out (30 s).
Every action tool takes an optional `tab` (a handle, §1.3), and every result names the tab it came
from (`tab`, or a `Tab: bN` line on the snapshot and screenshot).

| MCP tool | action | notes |
|---|---|---|
| `browserStatus` | — (Rust only) | every linked tab: handle, title, url, `opened_by`, which is `current`; or how to get linked |
| `browserNavigate` | `navigate` | `url`, or `back`/`forward`/`reload`; waits for load (≤15 s) |
| `browserSnapshot` | `snapshot` | accessibility tree as indented text; interactive/named nodes carry `[ref=eN]` |
| `browserClick` | `click` | `ref`; real mouse events at the element's centre, `this.click()` fallback |
| `browserType` | `type` | `ref` (optional: else the focused element), `text`, `clear` (default true), `submit` |
| `browserPressKey` | `press` | `key` (Enter, Tab, Escape, arrows, … or one character) |
| `browserScreenshot` | `screenshot` | returned as MCP image content (works for ssh agents too) |
| `browserEval` | `eval` | JS expression in the page, `returnByValue` |
| `browserConsole` | `console` | console messages, uncaught exceptions (`source` = file:line:col) and the browser's own `Log` entries (CSP, failed loads) since attach, last 200; `errors_only` |
| `browserNetwork` | `network` | requests since attach (last 300 kept, 100 shown; static assets hidden unless failed); `failed_only`, `filter`; `request_id` returns that response's body (20 000 chars) |

**The two purposes are diagnosis and driving**, in that order of how often they matter: an agent
debugging a web app reads the human's real page (its console errors, its failing API calls, the
API's error body) and then reproduces or verifies by acting in it. Capture starts at attach, so
the tools tell the agent to `reload` when the log is empty.

**A background tab must work as well as the front one.** Chrome delivers no synthetic mouse or
key input to a hidden page (verified: the click "succeeded" and no handler ran), so the driver
turns on `Emulation.setFocusEmulationEnabled` at attach; with it the page reads as visible and
focused and input lands. The agent never needs the human to switch to its tab.

**Another extension's frame locks maiTerm out** (2026-10-22, seen live on Google Cloud and
Cloudflare): Chrome refuses `chrome.debugger` on a tab holding another extension's frame ("Cannot
access a chrome-extension:// URL of different extension") and detaches a session the moment one
appears ("Detached while handling command"). In practice it is a password manager's autofill popup
(1Password), shown while a sign-in or secret field has focus. Nothing on our side can get past it.
So the driver retries the attach for 4 s (the popup usually closes on blur), then tells the agent
in plain words to ask its human to dismiss it (a reload needs the debugger too); a read that was
detached mid-command is repeated once, an action is reported as "may or may not have happened";
and a detach that isn't the human's Cancel keeps the console and network record. Turning off the
password manager's in-page autofill menu stops it at the source.

Refs are `e<backendDOMNodeId>`: stable for a node's life and needing no per-snapshot table, so a
ref from an older snapshot still works if the node still exists, and fails cleanly if not.

The tools are listed only while `Preferences.browser_link_enabled` is on (not in
`preference_meta`: an agent doesn't switch it on for itself).

## 5. Installing

Preferences → Chrome extension: the toggle writes the host manifests. Chrome lets no app install an
unpacked extension, so the install card IS the four manual steps (open `chrome://extensions`,
Developer mode, Load unpacked + the folder, pin the icon), with buttons for the parts maiTerm can do;
each refreshes the copy in the app data dir (`chrome-extension/`) first. Once connected the steps
fold away under "Install it in another browser". (Planned, not built: once the store listing is
live, the card links to it.) The pane shows each connected browser
and every live link.

**maiTerm shows a link where it lands**, since it is made in Chrome: a tab holding linked Chrome
tabs carries a browser mark in the tab strip (a count when several; the tooltip lists them by
handle), from `browserLinkStore`, which mirrors `browser_link_links` on every
`browser-link-changed`. A link the HUMAN makes (`link` with no opener) also emits
`browser-link-linked`, and the window holding that tab dispatches a notification ("Linked to
Chrome", deep-linked, so mute applies). A joined popup or a re-announce doesn't: a sign-in flow
or a reconnect would ring for nothing.
