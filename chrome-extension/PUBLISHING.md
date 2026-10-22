# Publishing the Chrome extension

Ported from `browser/safe-auto-refresh`, whose pipeline is proven. One hard limit: **the Chrome
Web Store API only uploads packages and flips the publish switch.** Listing copy, screenshots,
category and privacy answers are dashboard-only; [STORE_LISTING.md](STORE_LISTING.md) is the source
someone pastes in by hand.

| Automated (`.github/workflows/chrome-extension.yml`) | By hand, in the dashboard |
| --- | --- |
| Upload a new version on an `ext-v*` tag (as a draft) | Creating the item, once |
| Publish (trusted testers, everyone, staged %) | Listing copy, graphics, privacy answers |
| Tag/manifest version check, syntax check | The one-time $5 developer fee (if the account hasn't paid) |
| The store zip as a workflow artifact | Answering a review rejection |

## One-time setup

### 1. Create the item, keeping our extension id

maiTerm only accepts the extension whose id is `mdaccealpnngffgibmnijfgakddfgjpa`
(`ALLOWED_EXTENSION_IDS` in `src-tauri/src/browser_link/mod.rs`, also written into every native
host manifest). The store mints a new id for a new item **unless the first package carries the
private key as `key.pem`**, so the first upload is special:

```bash
chrome-extension/scripts/package.sh --with-key ~/.config/maiterm/chrome-extension-key.pem
```

1. In the [developer dashboard](https://chrome.google.com/webstore/devconsole/), under the
   publisher account maiTerm should ship from: **Add new item**, upload
   `dist/maiterm-chrome-<version>-with-key.zip`.
2. Check the item's id in the dashboard is `mdaccealpnngffgibmnijfgakddfgjpa`. If it isn't, stop:
   add the new id to `ALLOWED_EXTENSION_IDS` before anything ships, or no installed extension can
   connect.
3. Delete the `-with-key` zip. Every later upload is the plain package (the store refuses a second
   `key.pem`, and the key must never reach a user).
4. Fill in the listing from STORE_LISTING.md and submit.
5. The item id is `CWS_ITEM_ID`.

The private key lives at `~/.config/maiterm/chrome-extension-key.pem` (mode 600) and is in no
repo. Put a copy in the password manager: it is the only way to recreate the id.

### 2. OAuth client and refresh token

Reuse the Google Cloud project and OAuth client from safe-auto-refresh if you still have them
(its secrets are the same four names). Otherwise:

1. [Google Cloud console](https://console.cloud.google.com/) → APIs & Services → Library →
   enable the **Chrome Web Store API**.
2. Google Auth Platform (what "OAuth consent screen" opens) → **Get started**: any app name, your
   email, then Audience **Internal** if it's offered (a Workspace project: no expiry, no
   verification). Otherwise **External**, and afterwards Audience → **Publish app** ("In
   production"): in "Testing", refresh tokens expire after 7 days and the pipeline breaks with
   `invalid_grant` a week later. Data Access needs no scopes; the script asks for its one.
3. Clients → Create client → **Desktop app** (a Web client refuses the localhost redirect). Copy
   its client id and secret.
4. Mint the token with the account that owns the item:

```bash
CWS_CLIENT_ID=... CWS_CLIENT_SECRET=... node chrome-extension/scripts/get-refresh-token.mjs
```

### 3. Verify, then store the secrets

```bash
export CWS_CLIENT_ID=... CWS_CLIENT_SECRET=... CWS_REFRESH_TOKEN=... CWS_ITEM_ID=...
chrome-extension/scripts/package.sh
node chrome-extension/scripts/publish.mjs --dry-run
```

Then add all four to the GitHub repo (Flexmark-Intl/maiterm): Settings → Secrets and variables →
Actions.

## Releasing

```bash
# bump "version" in chrome-extension/manifest.json
git commit -am "Chrome extension 0.1.1: …"
git tag ext-v0.1.1
git push && git push origin ext-v0.1.1
```

The workflow checks the tag against the manifest, syntax-checks, builds the zip and uploads it as
a **draft**. It does not publish: that queues a review CI can't cancel. To publish, **Actions →
Chrome extension → Run workflow** with *publish* ticked (trusted testers first), or set the repo
variable `CWS_AUTO_PUBLISH=true`.

A protocol change between maiTerm and the extension (`PROTOCOL` in `browser_link/mod.rs`,
`PROTOCOL` in `background.js`) must ship in maiTerm and the extension together; the store review
takes days, so release the extension first when maiTerm can speak both.

## When it goes wrong

- **`invalid_grant`**: the consent screen is back in "Testing", or the token was revoked. Fix the
  screen, re-run `get-refresh-token.mjs`.
- **"Version already exists"**: bump `manifest.json`.
- **Green pipeline, nothing live**: expected; publishing submits for review.
- **Rejection**: arrives by email. The `debugger` justification in STORE_LISTING.md is the one a
  reviewer reads hardest.
