/**
 * aiTerm update-counter Worker
 *
 * Sits in front of the Tauri updater manifest (latest.json). Every update check
 * the app already makes (on launch + hourly, gated by the "auto check for
 * updates" preference) passes through here. We count it as an anonymous active
 * user and then serve the *real* latest.json unchanged.
 *
 * Privacy model (the Plausible model — no persistent identifier is stored):
 *   - We never store the IP address, User-Agent, or any cookie/device ID.
 *   - To de-duplicate the hourly pings into "unique users per day" we compute
 *     a salted hash:  uhash = SHA-256(daily_secret_salt | ip | user_agent).
 *   - The salt is random, rotates every day, and old salts are deleted — so a
 *     hash cannot be reversed to an IP, nor linked across days.
 *   - Users behind the same NAT collapse to one (privacy-favouring undercount).
 *
 * Robustness: this Worker is in the update-check path, so it fails safe. If the
 * upstream manifest can't be fetched it returns a non-2XX, and Tauri's updater
 * automatically falls through to the GitHub endpoint listed second in
 * tauri.conf.json. Logging happens in waitUntil() and can never delay or break
 * the update response.
 */

const ALLOWED_TARGET = new Set(["darwin", "linux", "windows"]);
const ALLOWED_ARCH = new Set(["x86_64", "aarch64", "i686", "armv7"]);
const VERSION_RE = /^[0-9]{1,3}(?:\.[0-9]{1,4}){0,3}$/;

function utcDay() {
  return new Date().toISOString().slice(0, 10); // YYYY-MM-DD (UTC)
}

function pick(value, allowed) {
  if (!value) return "unknown";
  const v = String(value).slice(0, 16);
  return allowed.has(v) ? v : "other";
}

function pickVersion(value) {
  const v = String(value || "").slice(0, 16);
  return VERSION_RE.test(v) ? v : "unknown";
}

async function sha256short(input) {
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
  let bin = "";
  for (const b of new Uint8Array(buf)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/[+/=]/g, "").slice(0, 22);
}

async function dailySalt(env, day) {
  const existing = await env.DB.prepare("SELECT value FROM salt WHERE day = ?").bind(day).first();
  if (existing?.value) return existing.value;
  const rnd = new Uint8Array(16);
  crypto.getRandomValues(rnd);
  const salt = [...rnd].map((b) => b.toString(16).padStart(2, "0")).join("");
  // INSERT OR IGNORE so concurrent requests on a fresh day don't fight.
  await env.DB.prepare("INSERT OR IGNORE INTO salt (day, value) VALUES (?, ?)").bind(day, salt).run();
  const settled = await env.DB.prepare("SELECT value FROM salt WHERE day = ?").bind(day).first();
  return settled?.value ?? salt;
}

async function logPing(env, request, meta) {
  try {
    const day = utcDay();
    const ip = request.headers.get("CF-Connecting-IP") || "";
    const ua = request.headers.get("User-Agent") || "";
    const salt = await dailySalt(env, day);
    const uhash = await sha256short(`${salt}|${ip}|${ua}`);
    await env.DB
      .prepare("INSERT OR IGNORE INTO pings (day, uhash, os, arch, version) VALUES (?, ?, ?, ?, ?)")
      .bind(day, uhash, meta.os, meta.arch, meta.version)
      .run();
  } catch {
    // Counting must never affect update delivery — swallow everything.
  }
}

async function handleStats(request, env) {
  // Header-only on purpose: the secret must never ride in a URL/query string,
  // where it could land in request logs, history, or Referer headers.
  const key = request.headers.get("x-stats-key");
  if (!env.STATS_KEY || key !== env.STATS_KEY) {
    return new Response("forbidden (use the x-stats-key header)\n", { status: 403 });
  }
  const [dau, mau, byVersion, byOs] = await Promise.all([
    env.DB.prepare(
      "SELECT day, COUNT(*) AS users FROM pings GROUP BY day ORDER BY day DESC LIMIT 30"
    ).all(),
    env.DB.prepare(
      "SELECT COUNT(DISTINCT uhash) AS users FROM pings WHERE day > date('now','-30 days')"
    ).first(),
    env.DB.prepare(
      "SELECT version, COUNT(DISTINCT uhash) AS users FROM pings WHERE day > date('now','-30 days') GROUP BY version ORDER BY users DESC"
    ).all(),
    env.DB.prepare(
      "SELECT os, COUNT(DISTINCT uhash) AS users FROM pings WHERE day > date('now','-30 days') GROUP BY os ORDER BY users DESC"
    ).all(),
  ]);
  return Response.json({
    mau_30d: mau?.users ?? 0,
    dau_last_30d: dau.results,
    by_version_30d: byVersion.results,
    by_os_30d: byOs.results,
  });
}

async function handleManifest(request, env, ctx, url) {
  let upstream;
  try {
    upstream = await fetch(env.UPSTREAM, {
      cf: { cacheTtl: 300, cacheEverything: true },
      redirect: "follow",
    });
  } catch {
    return new Response("upstream fetch failed\n", { status: 502 });
  }
  if (!upstream.ok) {
    // Non-2XX → Tauri updater falls through to the GitHub fallback endpoint.
    return new Response("upstream not ok\n", { status: 502 });
  }

  // Only count genuine update checks (the templated URL always carries ?v=).
  if (url.searchParams.has("v")) {
    ctx.waitUntil(
      logPing(env, request, {
        os: pick(url.searchParams.get("t"), ALLOWED_TARGET),
        arch: pick(url.searchParams.get("a"), ALLOWED_ARCH),
        version: pickVersion(url.searchParams.get("v")),
      })
    );
  }

  const body = await upstream.text();
  return new Response(body, {
    status: 200,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "public, max-age=300",
    },
  });
}

export default {
  async fetch(request, env, ctx) {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("method not allowed\n", { status: 405 });
    }
    const url = new URL(request.url);
    if (url.pathname === "/stats") return handleStats(request, env);
    // Everything else serves the manifest (the updater hits /latest.json).
    return handleManifest(request, env, ctx, url);
  },

  async scheduled(event, env, ctx) {
    // Keep only what's needed: 60 days of daily rows, 2 days of salts.
    ctx.waitUntil(
      (async () => {
        try {
          await env.DB.prepare("DELETE FROM pings WHERE day < date('now','-60 days')").run();
          await env.DB.prepare("DELETE FROM salt WHERE day < date('now','-2 days')").run();
        } catch {
          /* best effort */
        }
      })()
    );
  },
};
