// The maiTerm extension's service worker (docs/browser-link.md): finds each running maiTerm over
// native messaging, holds one WebSocket to each, keeps the human's links, and runs the actions
// maiTerm relays from an agent on its linked tabs. A Chrome tab is linked to one agent; an agent
// can hold several, each under a handle (b1, b2…) maiTerm gives it.

import { runAction, detach, forget } from './driver.js';
import { mark, unmark, retitle, releaseIfOurs, sweepOrphans } from './marker.js';

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

/** @returns {Promise<Array<{host, maitermTabId, maitermTabName, chromeTabId, handle, openedBy, title, url, placement?}>>} */
async function getLinks() {
  const { links } = await chrome.storage.session.get('links');
  return links ?? [];
}

async function setLinks(links) {
  // The highest handle each agent tab has had, per maiTerm: sent on `hello` so a restarted maiTerm
  // never gives a closed tab's handle to a new page.
  const { handleFloors = {} } = await chrome.storage.session.get('handleFloors');
  for (const l of links) {
    const n = Number(/^b(\d+)$/.exec(l.handle ?? '')?.[1] ?? 0);
    const key = `${l.host} ${l.maitermTabId}`;
    if (n > (handleFloors[key] ?? 0)) handleFloors[key] = n;
  }
  await chrome.storage.session.set({ links, handleFloors });
  await paintBadges(links);
}

async function handleFloorsFor(host) {
  const { handleFloors = {} } = await chrome.storage.session.get('handleFloors');
  const out = {};
  for (const [key, n] of Object.entries(handleFloors)) {
    const [h, id] = key.split(' ');
    if (h === host) out[id] = n;
  }
  return out;
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
    const handleFloors = await handleFloorsFor(host);
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
          for (const l of all.filter((l) => !kept.includes(l))) {
            await detach(l.chromeTabId);
            await unmark(l);
          }
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
          handleFloors,
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
    // The human, from maiTerm: show me that tab. Not an agent action, and needs no debugger.
    if (action === 'reveal') {
      try {
        const t = await chrome.tabs.update(chromeTabId, { active: true });
        await chrome.windows.update(t.windowId, { focused: true });
        return reply({ result: { ok: true } });
      } catch (e) {
        return reply({ error: String(e?.message ?? e) });
      }
    }
    const acts = OPENING_ACTIONS.has(action);
    // Its own token: two actions can start in the same millisecond, and one ending must not end both.
    const started = { at: Date.now() };
    if (acts) agentActing.set(chromeTabId, [...(agentActing.get(chromeTabId) ?? []), started]);
    try {
      reply({ result: await runAction(chromeTabId, action, args ?? {}) });
    } catch (e) {
      reply({ error: String(e?.message ?? e) });
    } finally {
      if (acts) {
        const left = (agentActing.get(chromeTabId) ?? []).filter((t) => t !== started);
        if (left.length) agentActing.set(chromeTabId, left);
        else agentActing.delete(chromeTabId);
        agentActed.set(chromeTabId, Date.now());
      }
    }
  } else if (msg.method === 'unlinked') {
    const { chromeTabId } = msg.params ?? {};
    await mutateLinks(async (links) => {
      const gone = links.filter((l) => l.host === host && l.chromeTabId === chromeTabId);
      if (!gone.length) return null;
      await detach(chromeTabId);
      for (const l of gone) await unmark(l);
      return links.filter((l) => !gone.includes(l));
    });
  } else if (msg.method === 'relinked') {
    // Its maiTerm tab was reloaded under a new id; the link follows it.
    const { chromeTabId, maitermTabId, maitermTabName } = msg.params ?? {};
    await mutateLinks(async (links) => {
      const l = links.find((x) => x.host === host && x.chromeTabId === chromeTabId);
      if (!l) return null;
      // Its handle floor moves with it (inside the queue: setLinks writes the same key).
      const { handleFloors = {} } = await chrome.storage.session.get('handleFloors');
      const from = `${host} ${l.maitermTabId}`;
      const to = `${host} ${maitermTabId}`;
      if (handleFloors[from] != null) {
        handleFloors[to] = Math.max(handleFloors[from], handleFloors[to] ?? 0);
        await chrome.storage.session.set({ handleFloors });
      }
      l.maitermTabId = maitermTabId;
      if (maitermTabName) l.maitermTabName = maitermTabName;
      await retitle(l);
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
// Groups our links no longer own: storage was cleared under them (§1.8). In the queue, so a link
// being made right now isn't swept before its placement is stored.
mutateLinks(async (links) => {
  await sweepOrphans(links);
  return null;
});

// ── linking (only from the popup: the human's click) ────────────────────────

/** Unlink a Chrome tab. `reason` tells its agent why: unlinked | closed | cancelled | moved. */
async function unlinkChromeTab(chromeTabId, reason = 'unlinked') {
  await mutateLinks(async (links) => {
    const gone = links.filter((l) => l.chromeTabId === chromeTabId);
    if (!gone.length) return null;
    for (const l of gone) await request(l.host, 'unlink', { chromeTabId, reason }).catch(() => {});
    await detach(chromeTabId);
    for (const l of gone) await unmark(l);
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
    const rest = all.filter((l) => !previous.includes(l));
    const entry = {
      host,
      maitermTabId: maitermTab.id,
      maitermTabName: maitermTab.name,
      chromeTabId: chromeTab.id,
      handle: r?.handle ?? '',
      openedBy: opener?.handle,
      title: chromeTab.title ?? '',
      url: chromeTab.url ?? '',
    };
    // Into its agent's tab group. A relink or a move between agents carries where the human had
    // it, as long as the tab is still where we put it (`mark` checks), so the final unlink puts it
    // back there.
    const carried = previous.find((l) => l.placement)?.placement;
    entry.placement = await mark(entry, rest, carried);
    return [...rest, entry];
  });
}

// ── tabs a linked page opens ────────────────────────────────────────────────
// A sign-in popup, or a page an agent's click opened, joins the agent of the tab that opened it:
// it comes from a page the human already handed over, and the flow can't finish without it. A tab
// the HUMAN opens from a linked page (a middle-click while the agent is idle) stays theirs: it
// joins only when it is a popup window, or the agent acted on the opener in the last few seconds.

/** Actions that can make a page open a tab. Reading (console, network, snapshot, screenshot)
 *  can't, so it never counts: an agent polling the console must not sweep up the tabs the human
 *  opens from that page. */
const OPENING_ACTIONS = new Set(['click', 'type', 'press', 'navigate', 'eval']);
/** chromeTabId → the opening actions running on it now ({at: start time}). One counts for at most
 *  ACTING_MAX_MS: an eval awaiting a promise that never settles must not keep every tab the human
 *  opens from that page joining the agent (maiTerm gave up on it after 30 s anyway). */
const agentActing = new Map();
const ACTING_MAX_MS = 30000;
/** chromeTabId → when an opening action on it last finished. A page opens its tab while the
 *  click is handled or just after; 2 s covers that without reaching a human's later middle-click. */
const agentActed = new Map();
const AGENT_OPENED_MS = 2000;

// The tab that really opened it. `tabs.onCreated`'s openerTabId can't be used: when a BACKGROUND
// tab opens one (the usual case, an agent working in a tab the human isn't looking at), Chrome
// reports the tab in FRONT as the opener. webNavigation names the source page itself.
chrome.webNavigation.onCreatedNavigationTarget.addListener(async ({ sourceTabId, tabId }) => {
  const opener = (await getLinks()).find((l) => l.chromeTabId === sourceTabId);
  if (!opener) return;
  const now = Date.now();
  const acting = (agentActing.get(sourceTabId) ?? []).some((a) => now - a.at < ACTING_MAX_MS);
  const recent = acting || now - (agentActed.get(sourceTabId) ?? 0) < AGENT_OPENED_MS;
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (!tab) return;
  const win = await chrome.windows.get(tab.windowId).catch(() => null);
  if (!recent && win?.type !== 'popup') return; // the human's own; the group guard below releases it
  await link(opener.host, { id: opener.maitermTabId, name: opener.maitermTabName }, tab, sourceTabId).catch(() => {});
});

// A tab that ends up in one of our groups without being linked to it (Chrome groups a tab opened
// from a grouped tab with it; "New tab to the right" too) would read as the agent's: take it out.
// After a moment, so a tab about to join its opener's agent isn't released first.
function guardGroup(tabId) {
  setTimeout(async () => {
    const links = await getLinks();
    if (links.some((l) => l.chromeTabId === tabId)) return;
    await releaseIfOurs({ id: tabId }, links);
  }, 600);
}
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.groupId != null && change.groupId >= 0) guardGroup(tabId);
});
// A tab Chrome CREATES inside a group never reports a change to it.
chrome.tabs.onCreated.addListener((tab) => {
  if (tab.groupId >= 0) guardGroup(tab.id);
});

chrome.tabs.onRemoved.addListener((tabId) => {
  forget(tabId);
  agentActed.delete(tabId);
  agentActing.delete(tabId);
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
