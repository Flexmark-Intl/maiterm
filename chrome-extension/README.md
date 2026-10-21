# maiTerm Chrome extension

Lets a maiTerm agent diagnose and drive one browser tab its human linked to it. The design,
the wire protocol and the rules are in [docs/browser-link.md](../docs/browser-link.md).

Plain Manifest V3 JavaScript, no build step:

- `background.js` — finds each running maiTerm over native messaging, holds a WebSocket to it,
  keeps the links, runs relayed actions.
- `driver.js` — every action, through `chrome.debugger` (DevTools Protocol).
- `popup.*` — the one place a link is made.

## Developing

1. In maiTerm (`npm run tauri:dev`): Preferences → Chrome extension → turn it on. That writes the
   native-messaging host manifest (`com.maiterm.browser_dev`) for every Chromium browser that has
   run on this machine.
2. `chrome://extensions` → Developer mode → Load unpacked → this folder. The `key` in
   `manifest.json` pins the id to `mdaccealpnngffgibmnijfgakddfgjpa`, the one maiTerm allows.
3. After editing, click the reload icon on the extension's card. Reloading clears its links
   (`chrome.storage.session`), so link the tab again.

Test without touching your own browser: Chrome for Testing (Playwright's) still honours
`--load-extension`. Give it its own `--user-data-dir` and copy the host manifest into
`<user-data-dir>/NativeMessagingHosts/`.

## Releasing

[PUBLISHING.md](PUBLISHING.md): tag `ext-v<version>` and `.github/workflows/chrome-extension.yml`
uploads a draft to the Chrome Web Store. The store item doesn't exist yet; its first upload must
carry the private key so the store keeps the id maiTerm trusts. [STORE_LISTING.md](STORE_LISTING.md)
and [PRIVACY.md](PRIVACY.md) are the dashboard copy.
