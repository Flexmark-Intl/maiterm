# Browser link — an agent diagnoses and drives ONE Chrome tab (2026-10-20)

A maiTerm agent tab can be linked to one tab in the human's own Chrome (or Brave, Edge, Arc —
any Chromium browser), and the agent then diagnoses and drives that tab over MCP: its console
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
   told to ask its human for one.
2. **An agent drives only its own link.** Every browser tool acts on the link of the CALLING
   tab, from its stated identity (`OWN_TAB_ONLY_TOOLS` + `PEER_ADDRESSING_TOOLS`): there is no
   `chromeTabId` argument and an explicit `tabId` naming another tab is refused.
3. **One Chrome tab per maiTerm tab, one maiTerm tab per Chrome tab.** Linking either side again
   replaces the old link.
4. **The human can always see and stop it.** Chrome's own "is debugging this browser" bar shows
   while a linked tab is attached. Cancelling that bar UNLINKS the tab (it is the human saying
   stop — re-attaching silently would overrule them). Closing the Chrome tab unlinks it.
5. **Links are in memory.** Chrome tab ids do not survive a browser restart, so nothing is
   persisted on a `Tab`. The extension keeps its links in `chrome.storage.session` (lives as long
   as the browser) and re-announces them on every (re)connect; maiTerm accepts a re-announced
   link only for a maiTerm tab that still exists. A maiTerm tab RELOAD carries its link to the new
   id (`browser_link::remap_tab`, called from `reloadTab`'s remaps). Every reload's old→new id is remembered (`Registry.renamed`), and both a re-announced
   link and the 5 s prune follow that chain, so a link that missed the remap (the extension was
   disconnected, or its `hello` was in flight) still reaches the reloaded tab.

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
| `hello` (first frame, within 5 s) | `{token, protocol, version, browser, links:[Link]}` | `{instance, protocol, accepted:[chromeTabId]}` |
| `listTabs` | — | `{tabs:[{id, name, workspace, window, runtime, linkedChromeTab?}]}` agent tabs only |
| `link` | `{maitermTabId, chromeTabId, title, url}` | `{ok}` / error |
| `unlink` | `{chromeTabId}` | `{ok}` |
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

`Link = {maitermTabId, chromeTabId, title, url}`.

A major-version mismatch in `protocol` is refused with a sentence the popup shows ("update the
extension" / "update maiTerm"). Additive changes don't bump it.

## 4. Actions and MCP tools

The extension does all CDP work (`chrome-extension/driver.js`); Rust relays and times out (30 s).

| MCP tool | action | notes |
|---|---|---|
| `browserStatus` | — (Rust only) | connected? linked? to which title/url; how to get linked |
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

Refs are `e<backendDOMNodeId>`: stable for a node's life and needing no per-snapshot table, so a
ref from an older snapshot still works if the node still exists, and fails cleanly if not.

The tools are listed only while `Preferences.browser_link_enabled` is on (not in
`preference_meta`: an agent doesn't switch it on for itself).

## 5. Installing

Preferences → Chrome extension: the toggle writes the host manifests; "Load unpacked" copies the
extension to the app data dir (`chrome-extension/`), reveals it and opens `chrome://extensions`.
Once a store listing exists the button opens it instead. The pane shows each connected browser
and every live link.
