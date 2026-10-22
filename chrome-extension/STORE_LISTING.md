# Chrome Web Store submission

Copy-paste source for the developer dashboard. Keep it in sync with `manifest.json`,
`PRIVACY.md` and `docs/browser-link.md`: if they disagree, the listing is wrong. The store
points at two website pages: `website/src/content/docs/features/chrome-extension.md` and
`website/src/content/docs/chrome-extension/privacy.md`, which is `PRIVACY.md` with a title, so
change both together.

Upload artifact: `dist/maiterm-chrome-<version>.zip` (`scripts/package.sh`). The FIRST upload is
`scripts/package.sh --with-key ~/.config/maiterm/chrome-extension-key.pem`, so the store keeps
the extension id maiTerm already trusts. See [PUBLISHING.md](PUBLISHING.md).

None of this can be set through the API; it is entered by hand once.

---

## Store listing

**Name**

```
maiTerm
```

**Summary** (132 characters max)

```
Let a maiTerm AI agent debug and drive one browser tab you link to it: console, network, page, clicks and typing.
```

**Category:** Developer Tools
**Language:** English (United States)

**Detailed description**

```
maiTerm for Chrome connects one browser tab to one AI agent running in maiTerm, the terminal for
working with coding agents (https://maiterm.dev). You choose the tab and the agent; the agent can
then diagnose and drive that tab, in your own browser with your own logins.

DIAGNOSE
- Console errors and warnings, with the file and line they came from
- Network requests: which failed, their status and timing, and the body an API returned
- The page as an accessibility tree: headings, text, links, buttons and form fields

DRIVE
- Click, type, press keys and navigate
- Screenshots
- Run JavaScript in the page

Typical uses: an agent fixing a bug reproduces it in your real, signed-in app and reads the
failing request; an agent checks its change in the browser instead of asking you to; an agent
walks a flow you would otherwise click through by hand.

YOU STAY IN CONTROL
- Only you link a tab, from this extension's icon. An agent cannot pick or reach any other tab.
- Each agent can use only the tab linked to it.
- Chrome shows its "is debugging this browser" bar while a tab is in use. Cancel it, close the
  tab, click Unlink, or turn the feature off in maiTerm, and the link ends.
- It works in the background: you can keep using other tabs while an agent works in its own.

REQUIRES maiTerm
The extension does nothing on its own. It talks only to the maiTerm app on your own computer
(127.0.0.1), which you turn on in maiTerm → Preferences → Chrome extension.

PRIVACY
No analytics, no telemetry, no servers of ours. What an agent reads from a linked tab goes to
your local maiTerm and to that agent. Full policy: see the privacy link on this page.
```

**Privacy policy URL**

```
https://maiterm.dev/chrome-extension/privacy/
```

**Homepage URL:** `https://maiterm.dev/features/chrome-extension/`

---

## Privacy practices tab

**Single purpose**

```
Lets an AI agent running in the maiTerm desktop app diagnose and operate one browser tab that the
user has explicitly linked to that agent.
```

**Permission justifications**

`debugger`

```
This is how the agent works in the tab the user linked, and only that tab. Through the DevTools
Protocol it reads the page's accessibility tree, console errors and network requests (including
a failing request's response body, which is how a developer finds why an API call broke), takes
screenshots, and sends clicks and key presses. Content scripts cannot do this: they cannot read
network traffic or uncaught errors from other scripts, and their synthetic events are untrusted,
so pages ignore them. The debugger is attached only to a tab the user linked through the
extension's popup; Chrome's "is debugging this browser" bar shows while it is attached, and
cancelling it unlinks the tab.
```

`nativeMessaging`

```
Used once per connection to ask the maiTerm desktop app, through its registered native host,
which local port it is listening on and for a per-launch secret. That is how the extension finds
the app and proves to it that it is this extension. No other data passes this way.
```

`tabs`

```
The popup needs the current tab's id, title and address to link it, and the extension shows a
badge on linked tabs and tells the app when a linked tab navigates or closes. Titles and
addresses are only ever sent to the user's own maiTerm app on 127.0.0.1, and only for linked tabs.
```

`storage`

```
Keeps the user's links (tab ids, titles, addresses, and the maiTerm agent tab each is linked to)
in session storage, so a link survives Chrome restarting the extension's background worker. It is
cleared when the browser closes.
```

`alarms`

```
Retries the connection to the maiTerm app every 30 seconds, so the extension reconnects when the
user starts maiTerm or the background worker restarts.
```

**Remote code:** No, I am not using remote code. (JavaScript the agent runs with its eval tool is
sent to the page through the DevTools Protocol, not executed in the extension.)

**Data usage** — tick:

- Website content (text, the page structure, network responses of a linked tab)
- Web history (titles and addresses of linked tabs)

Certify: not sold to third parties; not used for purposes unrelated to the single purpose; not
used for creditworthiness or lending.

---

## Graphics

Built by `node chrome-extension/scripts/store-assets.mjs` into `store-assets/` (1280×800): the
REAL popup in Chrome for Testing, talking to a stand-in maiTerm with a staged cast, so no real tab
names, hosts or paths reach a shot. Re-run it after any popup change.

- Store icon 128×128: `store-assets/store-icon-128.png` (96px artwork, 16px transparent padding)
- Screenshot 1: `store-assets/screenshot-1-link.png` — picking the agent
- Screenshot 2: `store-assets/screenshot-2-linked.png` — linked, beside a page with a failing request
- Small promo tile 440×280: `store-assets/promo-small-440x280.png`
- Marquee promo tile 1400×560: `store-assets/promo-marquee-1400x560.png`

The icon and tiles come from `python3 chrome-extension/scripts/store-brand.py`, drawn from the app
icon and `scripts/make-brand.py`'s wordmark, so a rebrand re-runs into them.
