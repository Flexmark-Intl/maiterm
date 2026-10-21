// Uploads the packaged zip to the Chrome Web Store and optionally publishes it.
//
//   node scripts/publish.mjs                        upload + publish publicly
//   node scripts/publish.mjs --upload-only          upload as a draft, do not publish
//   node scripts/publish.mjs --target=trustedTesters
//   node scripts/publish.mjs --percentage=20        staged rollout
//   node scripts/publish.mjs --dry-run              check credentials and preflight only
//
// Requires four environment variables — see chrome-extension/PUBLISHING.md for how to get
// them. No dependencies: plain fetch.
//
// Note the API only moves bytes and flips the publish switch. Listing copy,
// screenshots, promo tiles and the privacy disclosures are dashboard-only and
// cannot be automated. STORE_LISTING.md is the source for those.

import { readFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const TOKEN_URL = 'https://oauth2.googleapis.com/token';
const UPLOAD_URL = 'https://www.googleapis.com/upload/chromewebstore/v1.1/items';
const API_URL = 'https://www.googleapis.com/chromewebstore/v1.1/items';

// ---------------------------------------------------------------------------
// Arguments and environment
// ---------------------------------------------------------------------------

const args = process.argv.slice(2);
const flag = (name) => args.includes(`--${name}`);
const option = (name, fallback) => {
  const hit = args.find((a) => a.startsWith(`--${name}=`));
  return hit ? hit.slice(name.length + 3) : fallback;
};

const OPTIONS = {
  uploadOnly: flag('upload-only'),
  dryRun: flag('dry-run'),
  target: option('target', 'default'),
  percentage: option('percentage', null),
  zip: option('zip', null),
};

if (!['default', 'trustedTesters'].includes(OPTIONS.target)) {
  fail(`--target must be "default" or "trustedTesters", got "${OPTIONS.target}"`);
}

const REQUIRED = ['CWS_CLIENT_ID', 'CWS_CLIENT_SECRET', 'CWS_REFRESH_TOKEN', 'CWS_ITEM_ID'];

function readEnv() {
  const missing = REQUIRED.filter((key) => !process.env[key]);
  if (missing.length) {
    fail(
      `Missing credentials: ${missing.join(', ')}\n\n` +
        'These are set up once, by hand. See chrome-extension/PUBLISHING.md.\n' +
        'In CI they come from repository secrets of the same names.',
    );
  }
  return {
    clientId: process.env.CWS_CLIENT_ID,
    clientSecret: process.env.CWS_CLIENT_SECRET,
    refreshToken: process.env.CWS_REFRESH_TOKEN,
    itemId: process.env.CWS_ITEM_ID,
  };
}

function fail(message) {
  console.error(`\nerror: ${message}\n`);
  process.exit(1);
}

const step = (message) => console.log(`\n▸ ${message}`);

// ---------------------------------------------------------------------------
// Chrome Web Store API
// ---------------------------------------------------------------------------

async function getAccessToken({ clientId, clientSecret, refreshToken }) {
  const res = await fetch(TOKEN_URL, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      client_id: clientId,
      client_secret: clientSecret,
      refresh_token: refreshToken,
      grant_type: 'refresh_token',
    }),
  });

  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    const hint =
      body.error === 'invalid_grant'
        ? '\n\nA revoked or expired refresh token. The usual cause is an OAuth consent\n' +
          'screen left in "Testing" mode, which expires refresh tokens after 7 days.\n' +
          'Set it to "In production" in the Google Cloud console, then re-run\n' +
          '  node scripts/get-refresh-token.mjs'
        : '';
    fail(`Token exchange failed (${res.status} ${body.error ?? ''}): ${body.error_description ?? ''}${hint}`);
  }
  return body.access_token;
}

function apiHeaders(token) {
  return { authorization: `Bearer ${token}`, 'x-goog-api-version': '2' };
}

async function getItem(token, itemId) {
  const res = await fetch(`${API_URL}/${itemId}?projection=DRAFT`, {
    headers: apiHeaders(token),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    if (res.status === 404) {
      fail(
        `No Web Store item with ID ${itemId}.\n\n` +
          'The item has to be created by hand once, in the developer dashboard,\n' +
          'before the API can update it. See chrome-extension/PUBLISHING.md.',
      );
    }
    fail(`Could not read item ${itemId} (${res.status}): ${text.slice(0, 400)}`);
  }
  return res.json();
}

async function uploadZip(token, itemId, zipBytes) {
  const res = await fetch(`${UPLOAD_URL}/${itemId}`, {
    method: 'PUT',
    headers: { ...apiHeaders(token), 'content-type': 'application/zip' },
    body: zipBytes,
  });

  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    fail(`Upload failed (HTTP ${res.status}): ${JSON.stringify(body).slice(0, 600)}`);
  }
  if (body.uploadState === 'FAILURE') {
    const details = (body.itemError ?? [])
      .map((e) => `  - ${e.error_code}: ${e.error_detail}`)
      .join('\n');
    fail(`The store rejected the package:\n${details || JSON.stringify(body)}`);
  }
  return body;
}

async function publishItem(token, itemId, { target, percentage }) {
  const url = new URL(`${API_URL}/${itemId}/publish`);
  if (target !== 'default') url.searchParams.set('publishTarget', target);

  const res = await fetch(url, {
    method: 'POST',
    headers: {
      ...apiHeaders(token),
      ...(percentage ? { 'content-type': 'application/json' } : { 'content-length': '0' }),
    },
    ...(percentage ? { body: JSON.stringify({ deployPercentage: Number(percentage) }) } : {}),
  });

  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    fail(`Publish failed (HTTP ${res.status}): ${JSON.stringify(body).slice(0, 600)}`);
  }
  return body;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const manifest = JSON.parse(await readFile(join(ROOT, 'manifest.json'), 'utf8'));
const zipPath = OPTIONS.zip
  ? resolve(OPTIONS.zip)
  : join(ROOT, 'dist', `maiterm-chrome-${manifest.version}.zip`);

const zipBytes = await readFile(zipPath).catch(() =>
  fail(`No package at ${zipPath}\n\nBuild it first:  scripts/package.sh`),
);

console.log(`package  ${zipPath.replace(`${ROOT}/`, '')} (${(zipBytes.length / 1024).toFixed(1)} kB)`);
console.log(`version  ${manifest.version}`);

const env = readEnv();
console.log(`item     ${env.itemId}`);

step('Exchanging refresh token for an access token');
const token = await getAccessToken(env);
console.log('  ok');

step('Reading current item state');
const item = await getItem(token, env.itemId);
console.log(`  published version: ${item.crxVersion ?? '(none yet)'}`);
console.log(`  upload state:      ${item.uploadState ?? 'unknown'}`);

if (item.crxVersion === manifest.version) {
  fail(
    `Version ${manifest.version} is already the draft/published version in the store.\n\n` +
      'The store rejects re-uploads of the same version. Bump "version" in\n' +
      'chrome-extension/manifest.json, then re-run scripts/package.sh.',
  );
}

if (OPTIONS.dryRun) {
  console.log('\n--dry-run: credentials valid, package present, version is new. Stopping here.');
  process.exit(0);
}

step(`Uploading ${manifest.version}`);
const upload = await uploadZip(token, env.itemId, zipBytes);
console.log(`  uploadState: ${upload.uploadState}`);

if (OPTIONS.uploadOnly) {
  console.log(
    '\n--upload-only: the new version is saved as a draft.\n' +
      'Publish it from the dashboard, or re-run without --upload-only.',
  );
  process.exit(0);
}

step(`Publishing to "${OPTIONS.target}"${OPTIONS.percentage ? ` at ${OPTIONS.percentage}%` : ''}`);
const published = await publishItem(token, env.itemId, OPTIONS);

const statuses = published.status ?? [];
for (const [i, s] of statuses.entries()) {
  console.log(`  ${s}${published.statusDetail?.[i] ? ` — ${published.statusDetail[i]}` : ''}`);
}

// ITEM_PENDING_REVIEW is a normal, successful outcome: the submission landed
// and is queued. It is emphatically not "live".
const ok = statuses.length === 0 || statuses.every((s) => s === 'OK' || s === 'ITEM_PENDING_REVIEW');

console.log(
  ok
    ? '\nSubmitted. This queues a review — it does not make the version live.\n' +
        'Reviews take hours to days, and the outcome arrives by email, not here.\n' +
        `Dashboard: https://chrome.google.com/webstore/devconsole/`
    : '\nThe store did not accept the publish request. See the statuses above.',
);
process.exit(ok ? 0 : 1);
