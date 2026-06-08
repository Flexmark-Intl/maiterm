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
