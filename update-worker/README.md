# maiTerm update-counter

A small Cloudflare Worker that fronts the Tauri updater manifest (`latest.json`)
and counts update checks as anonymous active users.

- **Live at:** `https://updates.maiterm.dev/latest.json`
- **Account:** the project's Cloudflare account (ID kept out of this public repo — see the Cloudflare dashboard)
- **Upstream manifest:** `https://github.com/Flexmark-Intl/maiterm/releases/latest/download/latest.json`

The app points at this Worker first and the GitHub URL second
(`src-tauri/tauri.conf.json` → `plugins.updater.endpoints`). If the Worker ever
returns a non-2XX, the Tauri updater automatically falls through to GitHub, so
update delivery never depends on this service staying up.

## How counting works

Every check carries `?v={{current_version}}&t={{target}}&a={{arch}}`. The Worker:

1. Computes `uhash = SHA-256(daily_secret_salt | IP | user-agent)` — the salt is
   random, rotates daily, and old salts are pruned, so the hash is not reversible
   to an IP and cannot be linked across days (the Plausible model).
2. `INSERT OR IGNORE INTO pings (day, uhash, os, arch, version)` — the
   `(day, uhash)` primary key collapses the hourly pings to **one row per unique
   user per day**.
3. Serves the real `latest.json` byte-for-byte (the artifact signatures live
   inside the JSON and are unaffected).

No IP, user-agent, cookie, or device ID is ever stored. Users behind one NAT
collapse to a single count (a privacy-favouring undercount).

## Reading the numbers

`GET /stats` returns JSON (DAU for the last 30 days, 30-day MAU, breakdowns by
version and OS). The key is a Worker secret, passed in the **`x-stats-key`
header** (never a query string, so it can't leak into URLs/logs):

```bash
# reads the key from the local git-ignored file, so it stays out of shell history
curl -H "x-stats-key: $(grep -o '[a-f0-9]\{32\}' .stats-key.local)" \
  https://updates.maiterm.dev/stats
```

The key is saved locally to `.stats-key.local` (git-ignored). Rotate it any time
with `wrangler secret put STATS_KEY` and update that file.

Ad-hoc queries straight against the DB:

```bash
export CLOUDFLARE_API_TOKEN=...   # the cf token for the project's Cloudflare account
wrangler d1 execute aiterm_stats --remote \
  --command "SELECT day, COUNT(*) AS users FROM pings GROUP BY day ORDER BY day DESC LIMIT 14"
```

## maiLink doorbell relay (`POST /push`, `POST /push-capability`)

The same Worker doubles as the maiLink content-free push relay
(`docs/mailink-protocol.md` §6/§6.1). The maiTerm desktop POSTs a wake when a
maiLink-native tab needs a human **and** no phone holds a live LAN WebSocket; we
hold the secrets that can't live safely on every install (the Apple `.p8` and the
FCM service-account key) and fan out to APNs / FCM. **No terminal content ever
reaches the relay** — only the tab title + a `kind` (`permission` / `idle_done`)
ride along, which is all the alert shows. The phone wakes, opens its WS over
LAN/WireGuard, and pulls the real content.

**Multi-tenant.** One relay serves every user of the single published maiLink app,
so there is **no per-user shared secret** (it would have to ship in every install).
Instead each phone mints a per-device **capability** once, at pairing:

`POST /push-capability` — body `{push_token, platform}` → `{cap}`, where
`cap = base64url(HMAC-SHA256(CAP_SECRET, "<platform>:<push_token>"))`. The phone
hands `cap` to the desktops it pairs with (over the pinned-TLS LAN channel), and
the desktop presents it on every `/push`. `CAP_SECRET` never leaves the relay; a
desktop can't forge a cap for a token it never got from a real phone; rotating
`CAP_SECRET` revokes every cap at once. Stateless — no DB.

`POST /push` request (from the desktop, `application/json`):

```json
{ "push_token": "...", "platform": "apns", "env": "sandbox", "cap": "...",
  "tab_id": "...", "kind": "permission", "title": "tab name" }
```

- `cap`: the phone-minted capability for this `(platform, push_token)`. Required;
  `403 invalid capability` if missing or wrong.
- `platform`: `apns` (default) or `fcm`.
- `env`: only `"production"` routes to the APNs prod gateway
  (`api.push.apple.com`); anything else (incl. a dev build's `sandbox` token, or
  an unknown value) uses `api.sandbox.push.apple.com`.
- `collapse`/`thread` = `tab_id`, so repeat pings for one tab coalesce.

The response is JSON echoing the upstream verdict
(`{platform, ok, status, detail}`) so the desktop log shows APNs/FCM's own status
(e.g. `BadDeviceToken`) verbatim. `200` on success, `502` otherwise; `403` on a
bad capability, `503` if the relay isn't provisioned.

Secrets (all via `wrangler secret put` — see `wrangler.toml` for the list):
`CAP_SECRET`, `APNS_KEY_P8`, `APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_TOPIC`, and
(Android, optional) `FCM_SERVICE_ACCOUNT`.

Smoke test once the secrets are set — mint a cap for a throwaway token, then ring
it (expect `BadDeviceToken` from APNs, which proves cap+JWT+gateway all work):

```bash
CAP=$(curl -sS -X POST https://updates.maiterm.dev/push-capability \
  -H 'content-type: application/json' \
  -d '{"push_token":"0000","platform":"apns"}' | python3 -c 'import sys,json;print(json.load(sys.stdin)["cap"])')
curl -sS -X POST https://updates.maiterm.dev/push \
  -H 'content-type: application/json' \
  -d "{\"push_token\":\"0000\",\"platform\":\"apns\",\"env\":\"sandbox\",\"cap\":\"$CAP\",\"tab_id\":\"t1\",\"kind\":\"permission\",\"title\":\"smoke\"}"
```

## Deploy / update

```bash
export CLOUDFLARE_API_TOKEN="$(python3 -c "import tomllib;print(tomllib.load(open('$HOME/.cf/config.toml','rb'))['api_token'])")"
# Single-account token → wrangler resolves the account automatically.
# If you have multiple accounts, also: export CLOUDFLARE_ACCOUNT_ID=<account id>
wrangler deploy
```

## Daily prune

A cron trigger (`17 4 * * *`) runs the Worker's `scheduled()` handler to delete
`pings` older than 60 days and `salt` older than 2 days.

Gotcha for the record: cron triggers (and any `*.workers.dev` publishing) require
the Cloudflare **account** to have a registered workers.dev subdomain — a one-time
account-init step, unrelated to token scopes or which Worker route you use. This
account had never opened the Workers section, so the first cron deploy 403'd with
`10007 You do not have a workers.dev subdomain`. Fixed once by registering one
(`flexmark.workers.dev`); our Worker still serves only via the custom domain
(`workers_dev = false`). To prune manually if ever needed:

```bash
wrangler d1 execute aiterm_stats --remote \
  --command "DELETE FROM pings WHERE day < date('now','-60 days'); DELETE FROM salt WHERE day < date('now','-2 days');"
```
