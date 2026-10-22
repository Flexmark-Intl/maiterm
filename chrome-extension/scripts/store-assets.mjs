// Builds the Chrome Web Store screenshots (1280×800) into store-assets/.
//
//   node chrome-extension/scripts/store-assets.mjs
//
// The popup is the REAL extension in a real browser (Chrome for Testing, which still honours
// --load-extension), so a UI change that breaks the layout also breaks the shots. It talks to a
// stand-in maiTerm this script runs: a native-messaging host that hands out its address, and a
// WebSocket server speaking the extension's protocol (docs/browser-link.md §3) with a staged cast
// of agent tabs. Nothing of the machine's own maiTerm, tabs, names or paths reaches a shot.
//
// Needs Chrome for Testing in the Playwright cache: npx playwright install chromium

import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = join(ROOT, 'store-assets');
const EXT_ID = 'mdaccealpnngffgibmnijfgakddfgjpa';
const HOST = 'com.maiterm.browser';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The cast (the same names the website's and maiLink's shots use).
const TABS = [
  // In maiTerm's order: the front tab, then most recently used.
  { id: 't-web', name: 'web-ui', workspace: 'Storefront', runtime: 'claude', front: true },
  { id: 't-e2e', name: 'e2e checks', workspace: 'Storefront', runtime: 'claude' },
  { id: 't-rel', name: 'release-bot', workspace: 'Platform', runtime: 'claude', linked: [{ handle: 'b1', title: 'Staging · Orders' }, { handle: 'b2', title: 'Staging · Admin' }] },
  { id: 't-api', name: 'api-gateway', workspace: 'Platform', runtime: 'codex' },
  { id: 't-doc', name: 'docs-site', workspace: 'Docs', runtime: 'claude' },
].map((t) => ({ window: 'main', linked: [], front: false, ...t }));

const SHOTS = [
  {
    file: 'screenshot-1-link.png',
    state: 'picker',
    eyebrow: 'Link a tab',
    title: 'Pick the agent that may work in this tab.',
    sub: 'Only you link a tab, and each agent can use only the tabs linked to it.',
  },
  {
    file: 'screenshot-2-linked.png',
    state: 'linked',
    eyebrow: 'Diagnose and drive',
    title: 'Your agent reads the errors and works the page.',
    sub: 'Console, network, clicks and typing — in your browser, signed in as you.',
    page: true,
  },
];

// The page shot 2 is linked to: a staged app failing to load its data, the case an agent is
// linked in for. Served as a data: URL, so the address bar it would have is never on camera.
const DEMO_PAGE = `<!doctype html><title>Orders · Staging</title><style>
  body{margin:0;font:15px -apple-system,Helvetica,Arial,sans-serif;color:#1f2937;background:#fff}
  header{display:flex;align-items:center;gap:12px;padding:16px 28px;border-bottom:1px solid #e5e7eb}
  header b{font-size:17px} nav{display:flex;gap:18px;margin-left:24px;color:#6b7280} nav .on{color:#111827;font-weight:600}
  main{padding:24px 28px} h1{font-size:22px;margin:0 0 16px}
  .err{display:flex;gap:10px;align-items:flex-start;background:#fef2f2;border:1px solid #fecaca;color:#991b1b;border-radius:8px;padding:12px 14px;margin-bottom:18px}
  .err small{display:block;color:#b91c1c;opacity:.8;margin-top:2px}
  table{width:100%;border-collapse:collapse} th,td{text-align:left;padding:11px 8px;border-bottom:1px solid #f3f4f6}
  th{font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:#6b7280}
  .sk{height:12px;border-radius:6px;background:#f3f4f6}
  button{font:inherit;border:1px solid #d1d5db;background:#fff;border-radius:6px;padding:6px 12px}
</style><header><b>Acme Shop</b><nav><span>Dashboard</span><span class=on>Orders</span><span>Customers</span><span>Settings</span></nav></header>
<main><h1>Orders</h1><div class=err>⚠<div>Couldn’t load orders. <small>GET /api/orders?status=open — 500 Internal Server Error</small></div></div>
<table><tr><th>Order</th><th>Customer</th><th>Total</th><th>Status</th></tr>
${'<tr><td><div class=sk style="width:70px"></div></td><td><div class=sk style="width:140px"></div></td><td><div class=sk style="width:60px"></div></td><td><div class=sk style="width:80px"></div></td></tr>'.repeat(6)}
</table><p style="margin-top:18px"><button>Retry</button></p></main>`;

// ── a stand-in maiTerm: just enough WebSocket server for the extension ─────────

function wsServer(token) {
  const server = createServer();
  server.on('upgrade', (req, socket) => {
    if (req.headers.origin !== `chrome-extension://${EXT_ID}`) return socket.destroy();
    const accept = createHash('sha1')
      .update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11')
      .digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    const send = (obj) => {
      const body = Buffer.from(JSON.stringify(obj));
      const head = body.length < 126 ? Buffer.from([0x81, body.length]) : Buffer.from([0x81, 126, body.length >> 8, body.length & 255]);
      socket.write(Buffer.concat([head, body]));
    };
    let buf = Buffer.alloc(0);
    socket.on('data', (chunk) => {
      buf = Buffer.concat([buf, chunk]);
      for (;;) {
        if (buf.length < 2) return;
        let len = buf[1] & 127;
        let off = 2;
        if (len === 126) {
          if (buf.length < 4) return;
          len = buf.readUInt16BE(2);
          off = 4;
        }
        if (buf.length < off + 4 + len) return;
        const mask = buf.subarray(off, off + 4);
        const data = Buffer.from(buf.subarray(off + 4, off + 4 + len).map((b, i) => b ^ mask[i % 4]));
        const opcode = buf[0] & 15;
        buf = buf.subarray(off + 4 + len);
        if (opcode === 8) return socket.end();
        if (opcode !== 1) continue;
        const msg = JSON.parse(data.toString());
        const reply = (result) => send({ id: msg.id, result });
        if (msg.method === 'hello') {
          if (msg.params?.token !== token) return send({ id: msg.id, error: 'bad token' });
          reply({ instance: 'maiTerm', protocol: 1, accepted: (msg.params.links ?? []).map((l) => l.chromeTabId) });
        } else if (msg.method === 'listTabs') reply({ tabs: TABS, windows: 1 });
        else if (msg.method === 'link') reply({ ok: true, handle: 'b1' });
        else if (msg.id != null) reply({ ok: true });
      }
    });
  });
  return new Promise((r) => server.listen(0, '127.0.0.1', () => r({ server, port: server.address().port })));
}

// ── Chrome for Testing, with our host registered in its own profile ────────────

async function findChrome() {
  const cache = process.platform === 'darwin' ? join(homedir(), 'Library/Caches/ms-playwright') : join(homedir(), '.cache/ms-playwright');
  for (const d of (await readdir(cache).catch(() => [])).filter((d) => d.startsWith('chromium-')).sort().reverse()) {
    for (const p of [
      'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
      'chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
      'chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
      'chrome-linux64/chrome',
      'chrome-linux/chrome',
    ].map((rel) => join(cache, d, rel))) {
      if (await readFile(p).then(() => true, () => false)) return p;
    }
  }
  throw new Error('Chrome for Testing not found: run  npx playwright install chromium');
}

async function cdp(wsUrl) {
  const ws = new WebSocket(wsUrl);
  await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
  let n = 0;
  const waiting = new Map();
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    const w = waiting.get(m.id);
    if (w) (waiting.delete(m.id), m.error ? w.j(new Error(`${w.method}: ${m.error.message}`)) : w.r(m.result));
  };
  const call = (method, params = {}) =>
    new Promise((r, j) => {
      const id = ++n;
      waiting.set(id, { r, j, method });
      ws.send(JSON.stringify({ id, method, params }));
    });
  return { call, close: () => ws.close() };
}

async function evaluate(page, expression) {
  const r = await page.call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
  return r.result.value;
}

// ── composition ───────────────────────────────────────────────────────────────

function canvasHtml(shot, popupPng, size, pagePng) {
  // The popup is captured at 2x; drawn up to 1.35x it reads at store-thumbnail size, and never
  // taller than the room under the caption (about 560px).
  const k = Math.min(1.35, 560 / size.h);
  const w = Math.round(size.w * k);
  const h = Math.round(size.h * k);
  const page = pagePng
    ? `<div class="win"><div class="bar"><i></i><i></i><i></i></div><img class="page" src="data:image/png;base64,${pagePng}"></div>`
    : '';
  return `<!doctype html><html><head><style>
    html,body{margin:0;width:1280px;height:800px;overflow:hidden}
    body{font-family:-apple-system,'Helvetica Neue',Helvetica,Arial,sans-serif;
      background:radial-gradient(ellipse at 50% 0%,#f5f5f4 0%,#e7e5e4 70%,#d6d3d1 100%);
      display:flex;flex-direction:column;align-items:center;color:#1c1917}
    .eyebrow{margin-top:48px;font-size:15px;font-weight:700;letter-spacing:.14em;text-transform:uppercase;color:#78716c}
    h1{margin:10px 0 0;font-size:40px;font-weight:700;letter-spacing:-.01em;text-align:center;max-width:1120px}
    p{margin:10px 0 0;font-size:19px;color:#57534e;text-align:center}
    .stage{position:relative;margin-top:30px;display:flex;align-items:flex-start;justify-content:center;
      transform:translateX(-${pagePng ? Math.round(w * 0.55 / 2) : 0}px)}
    .shadow{box-shadow:0 24px 60px rgba(28,25,23,.18),0 2px 6px rgba(28,25,23,.08)}
    .popup{width:${w}px;height:${h}px;border-radius:12px;border:1px solid #e7e5e4}
    .win{width:720px;border-radius:10px;overflow:hidden;border:1px solid #d6d3d1;background:#fff}
    .bar{height:28px;background:#f5f5f4;border-bottom:1px solid #e7e5e4;display:flex;gap:7px;align-items:center;padding:0 12px}
    .bar i{width:11px;height:11px;border-radius:50%;background:#d6d3d1}
    .page{display:block;width:720px}
    .over{position:absolute;right:-${Math.round(w * 0.55)}px;top:40px}
  </style></head><body>
    <div class="eyebrow">${shot.eyebrow}</div><h1>${shot.title}</h1><p>${shot.sub}</p>
    <div class="stage">${page ? page.replace('class="win"', 'class="win shadow"') : ''}
      <img class="popup shadow${page ? ' over' : ''}" src="data:image/png;base64,${popupPng}"></div>
  </body></html>`;
}

// ── main ──────────────────────────────────────────────────────────────────────

// Found first: nothing is written to disk until we know there is a browser to run.
const chromePath = await findChrome();
const token = randomBytes(20).toString('hex');
const { server, port } = await wsServer(token);
const work = await mkdtemp(join(tmpdir(), 'maiterm-store-'));
const profile = join(work, 'profile');
await mkdir(join(profile, 'NativeMessagingHosts'), { recursive: true });

// The native host: answers the extension's discovery request with the stand-in's address.
const hostJs = join(work, 'host.mjs');
await writeFile(
  hostJs,
  `const chunks = [];
process.stdin.on('data', (c) => {
  chunks.push(c);
  const b = Buffer.concat(chunks);
  if (b.length >= 4 && b.length >= 4 + b.readUInt32LE(0)) {
    const out = Buffer.from(JSON.stringify(${JSON.stringify({ ok: true, port, token, instance: 'maiTerm', protocol: 1 })}));
    const len = Buffer.alloc(4); len.writeUInt32LE(out.length);
    process.stdout.write(Buffer.concat([len, out]), () => process.exit(0));
  }
});\n`,
);
const hostSh = join(work, 'host.sh');
await writeFile(hostSh, `#!/bin/sh\nexec "${process.execPath}" "${hostJs}"\n`);
await chmod(hostSh, 0o755);
await writeFile(
  join(profile, 'NativeMessagingHosts', `${HOST}.json`),
  JSON.stringify({ name: HOST, description: 'store-asset stand-in', path: hostSh, type: 'stdio', allowed_origins: [`chrome-extension://${EXT_ID}/`] }),
);

const chrome = spawn(chromePath, [
  `--user-data-dir=${profile}`,
  '--remote-debugging-port=0',
  `--load-extension=${ROOT}`,
  '--headless=new',
  '--no-first-run',
  '--no-default-browser-check',
  '--hide-scrollbars',
  'about:blank',
], { stdio: 'ignore' });

try {
  let devPort;
  for (let i = 0; i < 100 && !devPort; i++) {
    devPort = await readFile(join(profile, 'DevToolsActivePort'), 'utf8').then((s) => s.split('\n')[0], () => null);
    if (!devPort) await sleep(100);
  }
  const http = `http://127.0.0.1:${devPort}`;
  await mkdir(OUT, { recursive: true });

  const open = async (url) => {
    const t = await (await fetch(`${http}/json/new?${url}`, { method: 'PUT' })).json();
    const page = await cdp(t.webSocketDebuggerUrl);
    await page.call('Page.enable');
    return { ...page, id: t.id };
  };

  // Wait for the extension to connect to the stand-in.
  const popupUrl = `chrome-extension://${EXT_ID}/popup.html`;
  let page = await open(popupUrl);
  // Never shoot a popup that isn't connected: that would overwrite the committed shots with a
  // "not running" one and still exit 0.
  let status;
  for (let i = 0; i < 50 && status !== 'connected'; i++) {
    status = await evaluate(page, `chrome.runtime.sendMessage({type:'state'}).then(s=>s.instances.find(i=>i.host==='${HOST}')?.status)`).catch(() => null);
    if (status !== 'connected') await sleep(200);
  }
  if (status !== 'connected') throw new Error(`the extension never connected to the stand-in (last status: ${status}); no shots written`);

  for (const shot of SHOTS) {
    if (shot.state === 'linked') {
      // Link this very tab: the popup draws a link the same whatever page it is on.
      const linked = await evaluate(page, `(async()=>{const [t]=await chrome.tabs.query({active:true,currentWindow:true});
        return chrome.runtime.sendMessage({type:'link',host:'${HOST}',maitermTab:{id:'t-web',name:'web-ui'},chromeTab:{id:t.id,title:'Staging · Orders',url:'https://staging.example.com/orders'}})})()`);
      if (!linked?.ok) throw new Error(`linking failed: ${JSON.stringify(linked)}; stopped before ${shot.file}`);
    }
    await page.call('Emulation.setDeviceMetricsOverride', { width: 340, height: 900, deviceScaleFactor: 2, mobile: false });
    await page.call('Page.reload');
    await sleep(1200);
    await evaluate(page, `document.activeElement?.blur(), true`);
    const h = Math.ceil(await evaluate(page, `document.documentElement.getBoundingClientRect().height`));
    const shotPopup = await page.call('Page.captureScreenshot', { format: 'png', clip: { x: 0, y: 0, width: 340, height: h, scale: 1 } });

    const size = { w: 340, h };
    let pagePng;
    if (shot.page) {
      const demo = await open('about:blank');
      await demo.call('Emulation.setDeviceMetricsOverride', { width: 1080, height: 660, deviceScaleFactor: 1, mobile: false });
      const fid = (await demo.call('Page.getFrameTree')).frameTree.frame.id;
      await demo.call('Page.setDocumentContent', { frameId: fid, html: DEMO_PAGE });
      await sleep(400);
      pagePng = (await demo.call('Page.captureScreenshot', { format: 'png', clip: { x: 0, y: 0, width: 1080, height: 660, scale: 1 } })).data;
    }
    const canvas = await open('about:blank');
    await canvas.call('Emulation.setDeviceMetricsOverride', { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
    const frameId = (await canvas.call('Page.getFrameTree')).frameTree.frame.id;
    await canvas.call('Page.setDocumentContent', { frameId, html: canvasHtml(shot, shotPopup.data, size, pagePng) });
    await sleep(500);
    const out = await canvas.call('Page.captureScreenshot', { format: 'png', clip: { x: 0, y: 0, width: 1280, height: 800, scale: 1 } });
    await writeFile(join(OUT, shot.file), Buffer.from(out.data, 'base64'));
    console.log(`store-assets/${shot.file}  (popup ${size.w}×${size.h})`);
  }
} finally {
  chrome.kill();
  server.close();
  await sleep(300);
  await rm(work, { recursive: true, force: true });
}
