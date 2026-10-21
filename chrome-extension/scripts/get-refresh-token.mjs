// One-time helper: turns a Google OAuth client into a long-lived refresh token
// for the Chrome Web Store API.
//
//   CWS_CLIENT_ID=... CWS_CLIENT_SECRET=... node scripts/get-refresh-token.mjs
//
// Opens a consent page in your browser, catches the redirect on localhost, and
// prints the refresh token. Run it once; store the result as a secret.
//
// Google removed the out-of-band ("copy this code") flow in 2022, so this uses
// a loopback redirect. That requires the OAuth client to be of type
// "Desktop app" — a "Web application" client will reject http://localhost.

import { createServer } from 'node:http';
import { execFile } from 'node:child_process';

const SCOPE = 'https://www.googleapis.com/auth/chromewebstore';
const AUTH_URL = 'https://accounts.google.com/o/oauth2/v2/auth';
const TOKEN_URL = 'https://oauth2.googleapis.com/token';

const clientId = process.env.CWS_CLIENT_ID;
const clientSecret = process.env.CWS_CLIENT_SECRET;

if (!clientId || !clientSecret) {
  console.error(
    '\nerror: set CWS_CLIENT_ID and CWS_CLIENT_SECRET first.\n\n' +
      'Create them in the Google Cloud console:\n' +
      '  APIs & Services -> Credentials -> Create credentials -> OAuth client ID\n' +
      '  Application type: Desktop app\n\n' +
      'The "Chrome Web Store API" must be enabled on the same project, and the\n' +
      'OAuth consent screen must be set to "In production" — while it is in\n' +
      '"Testing" mode Google expires refresh tokens after 7 days.\n',
  );
  process.exit(1);
}

const code = await new Promise((resolveCode, rejectCode) => {
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    if (url.pathname !== '/') {
      res.writeHead(404).end();
      return;
    }

    const error = url.searchParams.get('error');
    const received = url.searchParams.get('code');

    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    res.end(
      `<!doctype html><meta charset="utf-8"><title>Chrome Web Store auth</title>
       <body style="font:16px system-ui;display:grid;place-items:center;height:100vh;margin:0">
       <p>${error ? `Authorisation failed: ${error}` : 'Done. Close this tab and return to the terminal.'}</p>`,
    );

    server.close();
    if (error) rejectCode(new Error(error));
    else if (received) resolveCode({ code: received, port: server.__port });
    else rejectCode(new Error('No authorisation code in the redirect'));
  });

  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.__port = port;

    const url = new URL(AUTH_URL);
    url.searchParams.set('client_id', clientId);
    url.searchParams.set('redirect_uri', `http://localhost:${port}`);
    url.searchParams.set('response_type', 'code');
    url.searchParams.set('scope', SCOPE);
    // access_type=offline + prompt=consent is what guarantees a refresh token
    // comes back. Without prompt=consent, a re-authorisation returns only an
    // access token and the script appears to silently do nothing useful.
    url.searchParams.set('access_type', 'offline');
    url.searchParams.set('prompt', 'consent');

    console.log('\nOpening the consent page. If it does not open, paste this:\n');
    console.log(`  ${url}\n`);
    execFile(process.platform === 'darwin' ? 'open' : 'xdg-open', [url.toString()], () => {});
  });
});

const res = await fetch(TOKEN_URL, {
  method: 'POST',
  headers: { 'content-type': 'application/x-www-form-urlencoded' },
  body: new URLSearchParams({
    client_id: clientId,
    client_secret: clientSecret,
    code: code.code,
    grant_type: 'authorization_code',
    redirect_uri: `http://localhost:${code.port}`,
  }),
});

const body = await res.json();
if (!res.ok || !body.refresh_token) {
  console.error(`\nerror: ${JSON.stringify(body, null, 2)}`);
  process.exit(1);
}

console.log('Refresh token (store this as the CWS_REFRESH_TOKEN secret):\n');
console.log(`  ${body.refresh_token}\n`);
console.log('Do not commit it. Verify the whole setup with:\n');
console.log('  node scripts/publish.mjs --dry-run\n');
