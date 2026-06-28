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

// ───────────────────────────── maiLink doorbell relay ─────────────────────────────
//
// POST /push is the maiLink content-free wake relay (docs/mailink-protocol.md §6/§6.1).
// The maiTerm desktop POSTs {push_token, platform, env, tab_id, kind, title} when a
// maiLink-native tab needs a human AND no phone holds a live LAN WebSocket. We hold the
// secrets that can't live safely on every desktop install (the APNs .p8 + the FCM
// service-account key) and fan out to Apple / Google. We NEVER see terminal content —
// only the tab title + kind ride along, which the contract permits in the alert.
//
// Stateless. The phone wakes, opens its WS over LAN/WireGuard, and pulls the real content.

function b64urlFromBytes(bytes) {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function b64urlFromString(str) {
  return b64urlFromBytes(new TextEncoder().encode(str));
}

function pemToDer(pem) {
  const body = pem
    .replace(/-----BEGIN [^-]+-----/, "")
    .replace(/-----END [^-]+-----/, "")
    .replace(/\s+/g, "");
  const bin = atob(body);
  const der = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) der[i] = bin.charCodeAt(i);
  return der;
}

// The doorbell is multi-tenant: ONE relay serves every maiTerm user of the one published
// maiLink app, so it can't authenticate desktops with a single shared secret (that secret
// would have to ship in every install). Instead each phone mints a per-device *capability*
// once, at pairing, via POST /push-capability — cap = HMAC(CAP_SECRET, platform:push_token).
// The phone hands the cap to the desktops it pairs with (over the pinned-TLS LAN channel);
// the desktop presents it on every /push. CAP_SECRET never leaves the relay, and a desktop
// can't forge a cap for a token it never received from a real phone. Stateless — no DB.
async function hmacCap(secret, platform, pushToken) {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const mac = await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(`${platform}:${pushToken}`)
  );
  return b64urlFromBytes(new Uint8Array(mac));
}

function timingSafeEqual(a, b) {
  if (typeof a !== "string" || typeof b !== "string" || a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

// APNs provider JWT (ES256). Apple wants this refreshed no more than once per ~20 min and
// no less than once per hour, so cache it in the isolate global and reuse for 30 min.
let _apnsJwtCache = { token: null, iat: 0, kid: null };

async function apnsJwt(env) {
  const kid = env.APNS_KEY_ID;
  const iss = env.APNS_TEAM_ID;
  const now = Math.floor(Date.now() / 1000);
  if (_apnsJwtCache.token && _apnsJwtCache.kid === kid && now - _apnsJwtCache.iat < 1800) {
    return _apnsJwtCache.token;
  }
  const key = await crypto.subtle.importKey(
    "pkcs8",
    pemToDer(env.APNS_KEY_P8),
    { name: "ECDSA", namedCurve: "P-256" },
    false,
    ["sign"]
  );
  const header = b64urlFromString(JSON.stringify({ alg: "ES256", kid }));
  const claims = b64urlFromString(JSON.stringify({ iss, iat: now }));
  const signingInput = `${header}.${claims}`;
  // WebCrypto ECDSA returns the raw r||s (IEEE P1363) signature JWS ES256 expects.
  const sig = await crypto.subtle.sign(
    { name: "ECDSA", hash: "SHA-256" },
    key,
    new TextEncoder().encode(signingInput)
  );
  const token = `${signingInput}.${b64urlFromBytes(new Uint8Array(sig))}`;
  _apnsJwtCache = { token, iat: now, kid };
  return token;
}

async function sendApns(env, msg) {
  if (!env.APNS_KEY_P8 || !env.APNS_KEY_ID || !env.APNS_TEAM_ID || !env.APNS_TOPIC) {
    return { ok: false, status: 503, detail: "apns not configured" };
  }
  // gateway-by-env: only "production" tokens go to the prod gateway; everything else
  // (sandbox dev builds, or an unknown/missing env) uses the sandbox gateway.
  const host =
    msg.env === "production" ? "api.push.apple.com" : "api.sandbox.push.apple.com";
  const jwt = await apnsJwt(env);
  const body = JSON.stringify({
    aps: {
      alert: {
        title: msg.title || "maiTerm",
        body: msg.kind === "permission" ? "Needs your approval" : "Agent finished",
      },
      sound: "default",
      "thread-id": msg.tab_id,
      "interruption-level": msg.kind === "permission" ? "time-sensitive" : "active",
    },
    tabId: msg.tab_id,
    kind: msg.kind,
  });
  const resp = await fetch(`https://${host}/3/device/${msg.push_token}`, {
    method: "POST",
    headers: {
      authorization: `bearer ${jwt}`,
      "apns-topic": env.APNS_TOPIC,
      "apns-push-type": "alert",
      "apns-priority": "10",
      "apns-collapse-id": String(msg.tab_id).slice(0, 64),
    },
    body,
  });
  const detail = await resp.text();
  return { ok: resp.ok, status: resp.status, detail: detail || "" };
}

// FCM HTTP v1 needs an OAuth2 access token minted from the service-account JWT (RS256).
let _fcmTokenCache = { token: null, exp: 0 };

async function fcmAccessToken(sa) {
  const now = Math.floor(Date.now() / 1000);
  if (_fcmTokenCache.token && now < _fcmTokenCache.exp - 60) return _fcmTokenCache.token;
  const header = b64urlFromString(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const claims = b64urlFromString(
    JSON.stringify({
      iss: sa.client_email,
      scope: "https://www.googleapis.com/auth/firebase.messaging",
      aud: sa.token_uri,
      iat: now,
      exp: now + 3600,
    })
  );
  const signingInput = `${header}.${claims}`;
  const key = await crypto.subtle.importKey(
    "pkcs8",
    pemToDer(sa.private_key),
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const sig = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    key,
    new TextEncoder().encode(signingInput)
  );
  const assertion = `${signingInput}.${b64urlFromBytes(new Uint8Array(sig))}`;
  const tokResp = await fetch(sa.token_uri, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
      assertion,
    }),
  });
  const tok = await tokResp.json();
  if (!tok.access_token) throw new Error(`fcm token mint failed: ${JSON.stringify(tok)}`);
  _fcmTokenCache = { token: tok.access_token, exp: now + (tok.expires_in || 3600) };
  return tok.access_token;
}

async function sendFcm(env, msg) {
  if (!env.FCM_SERVICE_ACCOUNT) {
    return { ok: false, status: 503, detail: "fcm not configured" };
  }
  const sa = JSON.parse(env.FCM_SERVICE_ACCOUNT);
  const accessToken = await fcmAccessToken(sa);
  const body = JSON.stringify({
    message: {
      token: msg.push_token,
      notification: {
        title: msg.title || "maiTerm",
        body: msg.kind === "permission" ? "Needs your approval" : "Agent finished",
      },
      android: { collapse_key: String(msg.tab_id), priority: "high" },
      data: { tabId: String(msg.tab_id), kind: String(msg.kind) },
    },
  });
  const resp = await fetch(
    `https://fcm.googleapis.com/v1/projects/${sa.project_id}/messages:send`,
    {
      method: "POST",
      headers: {
        authorization: `Bearer ${accessToken}`,
        "content-type": "application/json",
      },
      body,
    }
  );
  const detail = await resp.text();
  return { ok: resp.ok, status: resp.status, detail: detail || "" };
}

// POST /push-capability — a phone mints its per-device capability here, once, at pairing.
// Body {push_token, platform} → {cap}. Open by design (possessing the token is the gate;
// tokens are app-private and only minted by APNs/FCM for the real phone), but the cap keeps
// /push from being a blind open proxy and lets us revoke en masse by rotating CAP_SECRET.
async function handlePushCapability(request, env) {
  if (!env.CAP_SECRET) {
    return new Response("relay not configured\n", { status: 503 });
  }
  let body;
  try {
    body = await request.json();
  } catch {
    return new Response("bad json\n", { status: 400 });
  }
  if (!body || !body.push_token) {
    return new Response("missing push_token\n", { status: 400 });
  }
  const platform = body.platform === "fcm" ? "fcm" : "apns";
  const cap = await hmacCap(env.CAP_SECRET, platform, body.push_token);
  return Response.json({ cap });
}

async function handlePush(request, env, ctx) {
  // No shared per-user secret (this relay is multi-tenant). The desktop authenticates each
  // ring with the phone-minted capability instead. 503 (not 403) when CAP_SECRET is unset so
  // the desktop can tell "relay not provisioned yet" apart from "bad capability".
  if (!env.CAP_SECRET) {
    return new Response("relay not configured\n", { status: 503 });
  }
  let msg;
  try {
    msg = await request.json();
  } catch {
    return new Response("bad json\n", { status: 400 });
  }
  if (!msg || !msg.push_token || !msg.tab_id) {
    return new Response("missing push_token or tab_id\n", { status: 400 });
  }
  const platform = msg.platform === "fcm" ? "fcm" : "apns";
  const expectedCap = await hmacCap(env.CAP_SECRET, platform, msg.push_token);
  if (!msg.cap || !timingSafeEqual(msg.cap, expectedCap)) {
    return new Response("invalid capability\n", { status: 403 });
  }
  try {
    const result = platform === "fcm" ? await sendFcm(env, msg) : await sendApns(env, msg);
    // 200 with the upstream status inside, so the desktop log shows APNs/FCM's own verdict
    // (e.g. BadDeviceToken) without us guessing what's retryable.
    return Response.json({ platform, ...result }, { status: result.ok ? 200 : 502 });
  } catch (e) {
    return Response.json(
      { platform, ok: false, status: 500, detail: String(e && e.message ? e.message : e) },
      { status: 502 }
    );
  }
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/push") {
      if (request.method !== "POST") {
        return new Response("method not allowed\n", { status: 405 });
      }
      return handlePush(request, env, ctx);
    }
    if (url.pathname === "/push-capability") {
      if (request.method !== "POST") {
        return new Response("method not allowed\n", { status: 405 });
      }
      return handlePushCapability(request, env);
    }
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("method not allowed\n", { status: 405 });
    }
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
