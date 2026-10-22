// The maiTerm extension's service worker (docs/browser-link.md): finds each running maiTerm over
// native messaging, holds one WebSocket to each, keeps the human's links, and runs the actions
// maiTerm relays from an agent on its linked tabs. A Chrome tab is linked to one agent; an agent
// can hold several, each under a handle (b1, b2…) maiTerm gives it.

import { runAction, detach, forget } from './driver.js';

const PROTOCOL = 1;
const VERSION = chrome.runtime.getManifest().version;
/** One host per maiTerm build: an installed maiTerm and a dev one can both be linked. */
const HOSTS = ['com.maiterm.browser', 'com.maiterm.browser_dev'];
const PING_MS = 20000;

/**
 * host → { ws, status, instance, error, nextId, pending: Map<id, {resolve, reject}> }
 * status: connecting | connected | not_running | not_installed | refused | disconnected
 */
const conns = new Map(HOSTS.map((h) => [h, { status: 'disconnected', pending: new Map(), nextId: 1 }]));

// ── links (chrome.storage.session: survives a service-worker restart, not the browser) ─────

/** @returns {Promise<Array<{host, maitermTabId, maitermTabName, chromeTabId, handle, openedBy, title, url}>>} */
async function getLinks() {
  const { links } = await chrome.storage.session.get('links');
  return links ?? [];
}

async function setLinks(links) {
  await chrome.storage.session.set({ links });
  await paintBadges(links);
}

/** Every change to the links runs here, one at a time. Each one reads, awaits and writes, and two
 *  interleaved (a re-link racing maiTerm's `unlinked` for the old tab) lost the newer write. */
let linksQueue = Promise.resolve();
function mutateLinks(fn) {
  const run = linksQueue.then(async () => {
    const links = await getLinks();
    const next = await fn(links);
    if (next) await setLinks(next);
  });
  linksQueue = run.catch(() => {});
  return run;
}

async function paintBadges(links) {
  const tabs = await chrome.tabs.query({});
  for (const t of tabs) {
    // The handle the agent knows the tab by, so the human can tell which page it means.
    const link = links.find((l) => l.chromeTabId === t.id);
    chrome.action.setBadgeText({ tabId: t.id, text: link ? link.handle || 'ON' : '' }).catch(() => {});
  }
  chrome.action.setBadgeBackgroundColor({ color: '#78716c' }).catch(() => {});
}

function browserName() {
  if (navigator.brave) return 'Brave';
  const brands = navigator.userAgentData?.brands?.map((b) => b.brand) ?? [];
  if (brands.includes('Microsoft Edge')) return 'Edge';
  if (brands.includes('Opera')) return 'Opera';
  if (brands.includes('Google Chrome')) return 'Chrome';
  return 'Chromium';
}

// ── connections ─────────────────────────────────────────────────────────────

function request(host, method, params) {
  const c = conns.get(host);
  if (!c.ws || c.ws.readyState !== WebSocket.OPEN) return Promise.reject(new Error('not connected'));
  const id = c.nextId++;
  return new Promise((resolve, reject) => {
    c.pending.set(id, { resolve, reject });
    c.ws.send(JSON.stringify({ id, method, params }));
    setTimeout(() => {
      if (c.pending.delete(id)) reject(new Error('maiTerm did not answer'));
    }, 10000);
  });
}

async function connect(host) {
  const c = conns.get(host);
  if (c.ws && (c.ws.readyState === WebSocket.CONNECTING || c.ws.readyState === WebSocket.OPEN)) return;
  if (c.status === 'connecting') return;
  c.status = 'connecting';

  let disc;
  try {
    disc = await chrome.runtime.sendNativeMessage(host, { type: 'discover', version: VERSION });
  } catch (e) {
    // "Specified native messaging host not found": that maiTerm build never installed its host.
    c.status = 'not_installed';
    c.error = String(e?.message ?? e);
    return;
  }
  if (!disc?.ok) {
    c.status = 'not_running';
    c.error = disc?.error ?? 'maiTerm is not running';
    return;
  }

  const ws = new WebSocket(`ws://127.0.0.1:${disc.port}/browser-link`);
  c.ws = ws;
  ws.onopen = async () => {
    const links = (await getLinks()).filter((l) => l.host === host);
    const id = c.nextId++;
    c.pending.set(id, {
      resolve: async (r) => {
        c.status = 'connected';
        c.instance = r.instance;
        c.error = undefined;
        // Links maiTerm didn't take back (their tab closed meanwhile) are gone; the rest carry
        // the handle maiTerm kept or gave them.
        const accepted = new Set(r.accepted ?? []);
        const handles = r.handles ?? {};
        await mutateLinks(async (all) => {
          const kept = all.filter((l) => l.host !== host || accepted.has(l.chromeTabId));
          for (const l of all.filter((l) => !kept.includes(l))) await detach(l.chromeTabId);
          for (const l of kept) if (l.host === host) l.handle = handles[l.chromeTabId] ?? l.handle;
          return kept;
        });
      },
      reject: (e) => {
        c.status = 'refused';
        c.error = String(e?.message ?? e);
        ws.close();
      },
    });
    ws.send(
      JSON.stringify({
        id,
        method: 'hello',
        params: {
          token: disc.token,
          protocol: PROTOCOL,
          version: VERSION,
          browser: browserName(),
          links: links.map(({ maitermTabId, chromeTabId, handle, openedBy, title, url }) => ({ maitermTabId, chromeTabId, handle, openedBy, title, url })),
        },
      }),
    );
  };
  ws.onmessage = (ev) => onFrame(host, ev.data);
  ws.onclose = () => {
    if (c.ws !== ws) return;
    c.ws = undefined;
    if (c.status !== 'refused') c.status = 'disconnected';
    for (const p of c.pending.values()) p.reject(new Error('disconnected'));
    c.pending.clear();
  };
  ws.onerror = () => {};
}

async function onFrame(host, raw) {
  const c = conns.get(host);
  let msg;
  try {
    msg = JSON.parse(raw);
  } catch {
    return;
  }
  if (!msg.method) {
    const p = c.pending.get(msg.id);
    if (!p) return;
    c.pending.delete(msg.id);
    if (msg.error != null) p.reject(new Error(msg.error));
    else p.resolve(msg.result);
    return;
  }
  const reply = (body) => c.ws?.readyState === WebSocket.OPEN && c.ws.send(JSON.stringify({ id: msg.id, ...body }));
  if (msg.method === 'command') {
    const { chromeTabId, action, args } = msg.params ?? {};
    // maiTerm only relays for a link, but the link must be OURS and still to this instance.
    const link = (await getLinks()).find((l) => l.host === host && l.chromeTabId === chromeTabId);
    if (!link) return reply({ error: 'That Chrome tab is no longer linked.' });
    agentActed.set(chromeTabId, Date.now());
    try {
      reply({ result: await runAction(chromeTabId, action, args ?? {}) });
    } catch (e) {
      reply({ error: String(e?.message ?? e) });
    } finally {
      agentActed.set(chromeTabId, Date.now());
    }
  } else if (msg.method === 'unlinked') {
    const { chromeTabId } = msg.params ?? {};
    await mutateLinks(async (links) => {
      if (!links.some((l) => l.host === host && l.chromeTabId === chromeTabId)) return null;
      await detach(chromeTabId);
      return links.filter((l) => !(l.host === host && l.chromeTabId === chromeTabId));
    });
  } else if (msg.method === 'relinked') {
    // Its maiTerm tab was reloaded under a new id; the link follows it.
    const { chromeTabId, maitermTabId, maitermTabName } = msg.params ?? {};
    await mutateLinks((links) => {
      const l = links.find((x) => x.host === host && x.chromeTabId === chromeTabId);
      if (!l) return null;
      l.maitermTabId = maitermTabId;
      if (maitermTabName) l.maitermTabName = maitermTabName;
      return links;
    });
  }
}

async function ensureConnected() {
  await Promise.all(HOSTS.map((h) => connect(h).catch(() => {})));
}

// Keep the worker (and the sockets) alive: Chrome 116+ counts WebSocket traffic as activity.
setInterval(() => {
  for (const h of HOSTS) {
    if (conns.get(h).status === 'connected') request(h, 'ping').catch(() => {});
  }
}, PING_MS);

chrome.alarms.create('reconnect', { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener((a) => {
  if (a.name === 'reconnect') ensureConnected();
});
chrome.runtime.onStartup.addListener(ensureConnected);
chrome.runtime.onInstalled.addListener(ensureConnected);
ensureConnected();

// ── linking (only from the popup: the human's click) ────────────────────────

/** Unlink a Chrome tab. `reason` tells its agent why: unlinked | closed | cancelled | moved. */
async function unlinkChromeTab(chromeTabId, reason = 'unlinked') {
  await mutateLinks(async (links) => {
    const gone = links.filter((l) => l.chromeTabId === chromeTabId);
    if (!gone.length) return null;
    for (const l of gone) await request(l.host, 'unlink', { chromeTabId, reason }).catch(() => {});
    await detach(chromeTabId);
    return links.filter((l) => l.chromeTabId !== chromeTabId);
  });
}

/** Link a Chrome tab to an agent. An agent may hold several Chrome tabs; a Chrome tab has one
 *  agent, so linking it again moves it (§1.3). `openerChromeTabId`: it joins the agent of the
 *  linked tab that opened it. */
async function link(host, maitermTab, chromeTab, openerChromeTabId) {
  // Inside the queue, so a frame from maiTerm about this tab (queued behind the reply) applies
  // after the new link is written, not over it.
  await mutateLinks(async (all) => {
    const r = await request(host, 'link', {
      maitermTabId: maitermTab.id,
      chromeTabId: chromeTab.id,
      title: chromeTab.title ?? '',
      url: chromeTab.url ?? '',
      ...(openerChromeTabId != null ? { openerChromeTabId } : {}),
    });
    // This maiTerm already moved the tab on its side; another maiTerm holding it is told.
    const previous = all.filter((l) => l.chromeTabId === chromeTab.id);
    for (const l of previous) if (l.host !== host) await request(l.host, 'unlink', { chromeTabId: l.chromeTabId, reason: 'moved' }).catch(() => {});
    const opener = all.find((l) => l.chromeTabId === openerChromeTabId);
    return [
      ...all.filter((l) => !previous.includes(l)),
      {
        host,
        maitermTabId: maitermTab.id,
        maitermTabName: maitermTab.name,
        chromeTabId: chromeTab.id,
        handle: r?.handle ?? '',
        openedBy: opener?.handle,
        title: chromeTab.title ?? '',
        url: chromeTab.url ?? '',
      },
    ];
  });
}

// ── tabs a linked page opens ────────────────────────────────────────────────
// A sign-in popup, or a page an agent's click opened, joins the agent of the tab that opened it:
// it comes from a page the human already handed over, and the flow can't finish without it. A tab
// the HUMAN opens from a linked page (a middle-click while the agent is idle) stays theirs: it
// joins only when it is a popup window, or the agent acted on the opener in the last few seconds.

/** chromeTabId → when an agent's action on it last started or finished. */
const agentActed = new Map();
const AGENT_OPENED_MS = 5000;

chrome.tabs.onCreated.addListener(async (tab) => {
  if (tab.openerTabId == null) return;
  const opener = (await getLinks()).find((l) => l.chromeTabId === tab.openerTabId);
  if (!opener) return;
  const recent = Date.now() - (agentActed.get(tab.openerTabId) ?? 0) < AGENT_OPENED_MS;
  const win = await chrome.windows.get(tab.windowId).catch(() => null);
  if (!recent && win?.type !== 'popup') return;
  await link(opener.host, { id: opener.maitermTabId, name: opener.maitermTabName }, tab, opener.chromeTabId).catch(() => {});
});

chrome.tabs.onRemoved.addListener((tabId) => {
  forget(tabId);
  agentActed.delete(tabId);
  unlinkChromeTab(tabId, 'closed');
});

chrome.tabs.onUpdated.addListener(async (tabId, change, tab) => {
  if (!('title' in change) && !('url' in change)) {
    if (change.status === 'complete') paintBadges(await getLinks());
    return;
  }
  await mutateLinks((links) => {
    const mine = links.filter((l) => l.chromeTabId === tabId);
    if (!mine.length) return null;
    for (const l of mine) {
      l.title = tab.title ?? l.title;
      l.url = tab.url ?? l.url;
      request(l.host, 'tabUpdated', { chromeTabId: tabId, title: l.title, url: l.url }).catch(() => {});
    }
    return links;
  });
});

// The human cancelled Chrome's "is debugging this browser" bar: that is them saying stop (§1.4).
chrome.debugger.onDetach.addListener((source, reason) => {
  if (source.tabId == null) return;
  forget(source.tabId);
  if (reason === 'canceled_by_user') unlinkChromeTab(source.tabId, 'cancelled');
});

// ── popup ───────────────────────────────────────────────────────────────────

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  (async () => {
    switch (msg?.type) {
      case 'state': {
        await ensureConnected();
        const instances = HOSTS.map((h) => {
          const c = conns.get(h);
          return { host: h, status: c.status, instance: c.instance, error: c.error };
        });
        return { instances, links: await getLinks() };
      }
      case 'listTabs': {
        const out = [];
        for (const h of HOSTS) {
          const c = conns.get(h);
          if (c.status !== 'connected') continue;
          try {
            const r = await request(h, 'listTabs');
            out.push({ host: h, instance: c.instance, tabs: r.tabs ?? [], windows: r.windows ?? 1 });
          } catch (e) {
            out.push({ host: h, instance: c.instance, tabs: [], error: String(e?.message ?? e) });
          }
        }
        return { groups: out };
      }
      case 'link':
        await link(msg.host, msg.maitermTab, msg.chromeTab);
        return { ok: true };
      case 'unlink':
        await unlinkChromeTab(msg.chromeTabId);
        return { ok: true };
      default:
        return { error: 'unknown message' };
    }
  })().then(sendResponse, (e) => sendResponse({ error: String(e?.message ?? e) }));
  return true;
});
