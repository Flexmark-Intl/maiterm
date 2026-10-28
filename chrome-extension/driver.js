// The DevTools Protocol side of the browser link (docs/browser-link.md §4): every action an
// agent can take on its linked tab, run through chrome.debugger. maiTerm only relays.

const CDP_VERSION = '1.3';
const CONSOLE_KEEP = 200;
const NETWORK_KEEP = 300;
const BODY_MAX = 20000;
const LOAD_TIMEOUT_MS = 15000;

const attached = new Set();
/** tabId → console lines buffered since attach. */
const consoleBuf = new Map();
/** tabId → requestId → request, in arrival order (a Map keeps insertion order). */
const netLog = new Map();
/** tabId → the newest console message time recorded (ms), or when the agent cleared the buffer. */
const consoleLast = new Map();
/** tabId → console messages at or before this time are not recorded again. Set on a re-attach
 *  that kept the buffer: enabling Runtime and Log makes Chrome replay the page's stored messages. */
const consoleFloor = new Map();

/** tabId → requestId → {at, type}: requests in flight, for knowing when the page has settled. */
const inflight = new Map();
/** tabId → when the page last started or finished a request, or navigated (ms). */
const lastActivity = new Map();
/** tabId → when the top frame last navigated (ms). */
const lastNav = new Map();
/** Tabs whose link ended (unlinked, or the human cancelled Chrome's debugging bar) since the last
 *  command for them: a wait still running must not attach to them again. */
const released = new Set();

const send = (tabId, method, params = {}) => chrome.debugger.sendCommand({ tabId }, method, params);

/** Chrome refuses to debug a tab holding another extension's frame, and detaches a session the
 *  moment one appears. In practice that is a password manager's autofill popup (1Password,
 *  Bitwarden…), injected while a sign-in or secret field has focus, and gone when it loses it. */
const OTHER_EXTENSION = /chrome-extension:\/\/ URL of different extension/i;
const BLOCKED_HOW_LONG_MS = 4000;
const BLOCKED =
  "Another extension's frame is in this page, so Chrome won't let maiTerm in (it lets no extension debug a tab while one is there). " +
  "It is almost always a password manager's autofill popup (1Password, Bitwarden…), shown while a sign-in or secret field has focus. " +
  'Ask your human to dismiss it (Escape, or a click elsewhere on the page), then try again. A reload needs maiTerm in the page too, so it is no way round this.';

export async function ensureAttached(tabId) {
  if (attached.has(tabId)) return;
  for (const start = Date.now(); ; ) {
    try {
      await chrome.debugger.attach({ tabId }, CDP_VERSION);
      break;
    } catch (e) {
      const msg = String(e?.message ?? e);
      // Attached already (a service-worker restart forgot our Set): carry on.
      if (/already attached/i.test(msg)) break;
      if (!OTHER_EXTENSION.test(msg)) throw new Error(`Could not attach to the tab: ${msg}`);
      // The popup usually closes on its own when the field loses focus.
      if (Date.now() - start > BLOCKED_HOW_LONG_MS) throw new Error(BLOCKED);
      await sleep(400);
    }
  }
  // Unlinked while the attach was retrying (another extension's frame held it up for seconds): let
  // go at once, or Chrome's debugging bar stays on a tab nobody holds.
  if (released.has(tabId)) {
    await chrome.debugger.detach({ tabId }).catch(() => {});
    throw new Error('That Chrome tab is no longer linked.');
  }
  attached.add(tabId);
  // Whatever was in flight before this session will never report its end to it.
  inflight.set(tabId, new Map());
  if (consoleBuf.has(tabId)) {
    // Re-attached with the record kept (§4): skip the replay, and mark requests that were in
    // flight while detached, whose ends this session will never report.
    consoleFloor.set(tabId, consoleLast.get(tabId) ?? 0);
    for (const r of netLog.get(tabId)?.values() ?? []) {
      if (r.status == null && r.failed == null && r.ms == null) r.failed = 'unknown: maiTerm was detached from the tab while this was in flight';
    }
  } else {
    consoleBuf.set(tabId, []);
    consoleFloor.delete(tabId);
    consoleLast.delete(tabId);
  }
  if (!netLog.has(tabId)) netLog.set(tabId, new Map());
  await send(tabId, 'Runtime.enable');
  await send(tabId, 'Page.enable');
  await send(tabId, 'DOM.enable');
  await send(tabId, 'Network.enable');
  await send(tabId, 'Log.enable');
  // The agent usually works in a tab the human isn't looking at. Chrome delivers no mouse or key
  // input to a hidden page, and pauses its timers; focus emulation makes it behave as the
  // focused, visible tab (verified: clicks and keys land, visibilityState reads "visible").
  await send(tabId, 'Emulation.setFocusEmulationEnabled', { enabled: true });
}

/** The link ended or moved to another agent without the tab being detached: a wait running for
 *  the old link ends instead of reporting the page to an agent that no longer holds it. */
export function release(tabId) {
  released.add(tabId);
}

export async function detach(tabId) {
  released.add(tabId);
  attached.delete(tabId);
  consoleBuf.delete(tabId);
  netLog.delete(tabId);
  forgetActivity(tabId);
  try {
    await chrome.debugger.detach({ tabId });
  } catch {
    // not attached
  }
}

/** Chrome detached us. `keepLogs` when the tab is still linked (another extension's frame pushed
 *  us out): the console and network record so far is what the agent came for, and the next
 *  action re-attaches and carries on adding to it. */
export function forget(tabId, { keepLogs = false } = {}) {
  attached.delete(tabId);
  forgetActivity(tabId);
  if (keepLogs) return;
  released.add(tabId);
  consoleBuf.delete(tabId);
  netLog.delete(tabId);
  consoleLast.delete(tabId);
  consoleFloor.delete(tabId);
}

function pushConsole(tabId, line, ts) {
  const buf = consoleBuf.get(tabId);
  if (!buf) return;
  if (ts <= (consoleFloor.get(tabId) ?? -Infinity)) return; // replayed on re-attach, or cleared
  if (ts > (consoleLast.get(tabId) ?? -Infinity)) consoleLast.set(tabId, ts);
  buf.push(line);
  if (buf.length > CONSOLE_KEEP) buf.splice(0, buf.length - CONSOLE_KEEP);
}

function remoteObjectText(o) {
  if (!o) return '';
  if ('value' in o) return typeof o.value === 'string' ? o.value : JSON.stringify(o.value);
  return o.description ?? o.unserializableValue ?? o.type;
}

/** "app.js:12:5" for the top frame of a stack, so an agent can go straight to the source. */
function where(stack, url, line, col) {
  const f = stack?.callFrames?.[0];
  const u = f?.url || url;
  if (!u) return undefined;
  const l = f ? f.lineNumber : line;
  const c = f ? f.columnNumber : col;
  return l == null ? u : `${u}:${l + 1}:${(c ?? 0) + 1}`;
}

function forgetActivity(tabId) {
  inflight.delete(tabId);
  lastActivity.delete(tabId);
  lastNav.delete(tabId);
}

/** Requests that can change what the page shows. Images, fonts, beacons and the like are left out:
 *  a page that lazy-loads images or pings analytics as it scrolls would otherwise never settle. */
const CONTENT_REQUESTS = new Set(['Document', 'XHR', 'Fetch', 'Script']);

function onActivity(tabId, method, p) {
  const live = inflight.get(tabId);
  if (!live) return;
  if (method === 'Network.requestWillBeSent') {
    if (!CONTENT_REQUESTS.has(p.type)) return;
    live.set(p.requestId, { at: Date.now() });
  } else if (method === 'Network.loadingFinished' || method === 'Network.loadingFailed') {
    if (!live.delete(p.requestId)) return;
  } else if (method === 'Page.frameNavigated') {
    if (p.frame?.parentId) return;
    lastNav.set(tabId, Date.now());
  } else return;
  lastActivity.set(tabId, Date.now());
}

function onNetwork(tabId, method, p) {
  const log = netLog.get(tabId);
  if (!log) return;
  if (method === 'Network.requestWillBeSent') {
    log.set(p.requestId, {
      id: p.requestId,
      method: p.request.method,
      url: p.request.url,
      type: p.type,
      started: p.wallTime ? new Date(p.wallTime * 1000).toISOString() : undefined,
      t0: p.timestamp,
    });
    if (log.size > NETWORK_KEEP) log.delete(log.keys().next().value);
    return;
  }
  const r = log.get(p.requestId);
  if (!r) return;
  if (method === 'Network.responseReceived') {
    r.status = p.response.status;
    r.statusText = p.response.statusText || undefined;
    r.mimeType = p.response.mimeType;
    r.fromCache = p.response.fromDiskCache || p.response.fromServiceWorker || undefined;
  } else if (method === 'Network.loadingFinished') {
    r.bytes = p.encodedDataLength;
    r.ms = Math.round((p.timestamp - r.t0) * 1000);
  } else if (method === 'Network.loadingFailed') {
    r.failed = p.canceled ? 'canceled' : p.blockedReason ? `blocked: ${p.blockedReason}` : p.errorText;
    r.ms = Math.round((p.timestamp - r.t0) * 1000);
  }
}

chrome.debugger.onEvent.addListener((source, method, params) => {
  const tabId = source.tabId;
  if (tabId == null) return;
  onActivity(tabId, method, params);
  if (method.startsWith('Network.')) {
    onNetwork(tabId, method, params);
  } else if (method === 'Runtime.consoleAPICalled') {
    const text = (params.args ?? []).map(remoteObjectText).join(' ');
    pushConsole(tabId, { level: params.type, text, source: where(params.stackTrace), at: new Date(params.timestamp).toISOString() }, params.timestamp);
  } else if (method === 'Runtime.exceptionThrown') {
    const d = params.exceptionDetails ?? {};
    const text = d.exception?.description ?? d.text ?? 'Uncaught exception';
    pushConsole(tabId, { level: 'exception', text, source: where(d.stackTrace, d.url, d.lineNumber, d.columnNumber), at: new Date(params.timestamp).toISOString() }, params.timestamp);
  } else if (method === 'Log.entryAdded') {
    // Browser-side messages: CSP violations, failed resource loads, deprecations.
    const e = params.entry;
    pushConsole(tabId, { level: e.level, text: e.text, source: e.url, origin: e.source, at: new Date(e.timestamp).toISOString() }, e.timestamp);
  }
});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function tabInfo(tabId) {
  const t = await chrome.tabs.get(tabId);
  return { url: t.url ?? '', title: t.title ?? '' };
}

/** Wait for the tab to finish loading: it must be seen loading first, or stay complete a second. */
async function waitForLoad(tabId) {
  const start = Date.now();
  let sawLoading = false;
  while (Date.now() - start < LOAD_TIMEOUT_MS) {
    const t = await chrome.tabs.get(tabId);
    if (t.status === 'loading') sawLoading = true;
    else if (t.status === 'complete' && (sawLoading || Date.now() - start > 1000)) return true;
    await sleep(150);
  }
  return false;
}

/** No content request for this long after the last one ended, nothing loading: the page has settled. */
const QUIET_MS = 400;
/** A request open this long is a long poll or a stream, not the page still loading. */
const LONG_LIVED_MS = 4000;
/** How long past the action (a navigation's load included) settling may take. */
const SETTLE_MAX_MS = 5000;

/** Wait until the page has settled after an action begun at `since`, as Chrome reports it (the tab's
 *  load, and the requests in flight), for up to `max` ms after `from` (when the action itself
 *  returned). A page that keeps polling never settles; then it says so, and the agent looks for
 *  itself. */
async function settle(tabId, since, max, from = since) {
  for (;;) {
    const now = Date.now();
    const t = await chrome.tabs.get(tabId).catch(() => null);
    const busy = [...(inflight.get(tabId)?.values() ?? [])].filter((r) => now - r.at < LONG_LIVED_MS).length;
    const quietFor = now - Math.max(lastActivity.get(tabId) ?? 0, since);
    const navigated = (lastNav.get(tabId) ?? 0) >= since;
    if (!t || (t.status === 'complete' && busy === 0 && quietFor >= QUIET_MS)) {
      return { navigated, settled: true, waited_ms: now - since };
    }
    if (now - from >= max) {
      return {
        navigated,
        settled: false,
        waited_ms: now - since,
        note: t.status === 'loading'
          ? 'The page was still loading when maiTerm stopped waiting: take a snapshot, or browserWaitFor what you expect.'
          : "The page kept making requests and never went quiet (it may poll constantly), so maiTerm stopped waiting: take a snapshot, or browserWaitFor what you expect.",
      };
    }
    await sleep(50);
  }
}

// Under maiTerm's 30 s relay timeout, with room for a re-attach held up by another extension's frame.
const WAIT_MAX_S = 18;

/** In the page: resolve true once its text has (or, with gone, no longer has) `text`, watching DOM
 *  changes rather than polling; false after `ms`. Checks at most every 100 ms, since innerText
 *  lays the page out. */
const WAIT_FOR_TEXT = `function (text, gone, ms) {
  return new Promise((resolve) => {
    const ok = () => (document.body ? document.body.innerText : '').includes(text) !== gone;
    if (ok()) return resolve(true);
    let pending = false;
    const obs = new MutationObserver(() => {
      if (pending) return;
      pending = true;
      setTimeout(() => { pending = false; if (ok()) { done(); resolve(true); } }, 100);
    });
    const timer = setTimeout(() => { done(); resolve(ok()); }, ms);
    const done = () => { obs.disconnect(); clearTimeout(timer); };
    // Attributes too: text is often shown or hidden by a class or style change.
    obs.observe(document, { subtree: true, childList: true, characterData: true, attributes: true });
  });
}`;

/** Wait for text to appear or go, or the URL to contain something, across navigations. */
async function waitFor(tabId, args) {
  const text = args.text != null && String(args.text) !== '' ? String(args.text) : null;
  const url = args.url != null && String(args.url) !== '' ? String(args.url) : null;
  if (text == null && url == null) throw new Error('Say what to wait for: text (with gone: true to wait for it to disappear), url, or both.');
  const gone = args.gone === true;
  const timeoutMs = Math.min(Math.max(Number(args.timeout ?? 10), 0.5), WAIT_MAX_S) * 1000;
  const start = Date.now();
  for (;;) {
    if (released.has(tabId)) {
      return { met: false, waited_ms: Date.now() - start, note: 'The tab was unlinked while waiting.' };
    }
    const info = await tabInfo(tabId);
    const urlOk = url == null || info.url.includes(url);
    let textOk = text == null;
    if (urlOk && text != null) {
      try {
        // Chrome may have cut maiTerm off meanwhile (a password manager's frame while the human
        // signs in, which is often what is being waited for): come back in once it lets us.
        await ensureAttached(tabId);
        // Taken after the attach, which can itself take seconds, so the slice stays inside the timeout.
        const remaining = timeoutMs - (Date.now() - start);
        // Give the page a slice at a time: a navigation ends this context, and the next slice runs in the new page.
        const { result: doc } = await send(tabId, 'Runtime.evaluate', { expression: 'document' });
        const { result } = await send(tabId, 'Runtime.callFunctionOn', {
          objectId: doc.objectId,
          functionDeclaration: WAIT_FOR_TEXT,
          arguments: [{ value: text }, { value: gone }, { value: Math.max(0, Math.min(remaining, 2000)) }],
          awaitPromise: true,
          returnByValue: true,
        });
        textOk = result.value === true;
      } catch (e) {
        if (released.has(tabId)) continue; // unlinked during the attach: the check above answers
        // The page navigated mid-wait (look again in the new one), or Chrome detached maiTerm.
        if (/not attached|detached/i.test(String(e?.message ?? e))) attached.delete(tabId);
        await sleep(e?.message === BLOCKED ? 400 : 100);
      }
    }
    if (urlOk && textOk) return { met: true, waited_ms: Date.now() - start, ...(await tabInfo(tabId)) };
    if (Date.now() - start >= timeoutMs) {
      const what = [text != null ? `${gone ? 'no' : ''} "${text}" on the page`.trim() : null, url != null ? `a URL containing "${url}"` : null].filter(Boolean).join(' and ');
      return { met: false, waited_ms: Date.now() - start, note: `Gave up after ${Math.round(timeoutMs / 1000)} s waiting for ${what}.`, ...(await tabInfo(tabId)) };
    }
    if (!urlOk) await sleep(100);
  }
}

function refToNodeId(ref) {
  const m = /^e?(\d+)$/.exec(String(ref ?? '').trim());
  if (!m) throw new Error(`"${ref}" is not a ref from browserSnapshot (they look like e42).`);
  return Number(m[1]);
}

/** CDP's "no such node" errors, said in terms the agent can act on. */
function staleRef(e) {
  const msg = String(e?.message ?? e);
  if (/No node|Could not find node|does not belong|Node is detached/i.test(msg)) {
    return new Error('That element is no longer on the page: take a new browserSnapshot.');
  }
  return e instanceof Error ? e : new Error(msg);
}

async function objectIdFor(tabId, backendNodeId) {
  const { object } = await send(tabId, 'DOM.resolveNode', { backendNodeId });
  return object.objectId;
}

// ── snapshot ────────────────────────────────────────────────────────────────

/** Roles an agent acts on: these always get a ref. */
const INTERACTIVE = new Set([
  'button', 'link', 'textbox', 'searchbox', 'combobox', 'checkbox', 'radio', 'switch', 'slider',
  'spinbutton', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option', 'tab', 'treeitem',
  'listbox', 'gridcell', 'columnheader', 'DisclosureTriangle',
]);
/** Wrappers that add nothing when unnamed: their children are lifted up a level. */
const TRANSPARENT = new Set(['none', 'generic', 'GenericContainer', 'InlineTextBox', 'LineBreak', 'Ignored', 'presentation']);
const STATE_PROPS = ['focused', 'checked', 'pressed', 'expanded', 'selected', 'disabled', 'required', 'readonly'];

function prop(node, name) {
  return node.properties?.find((p) => p.name === name)?.value?.value;
}

async function snapshot(tabId, args) {
  const max = Math.max(2000, Number(args.max_chars) || 40000);
  const { nodes } = await send(tabId, 'Accessibility.getFullAXTree');
  const byId = new Map(nodes.map((n) => [n.nodeId, n]));
  const root = nodes.find((n) => !n.parentId) ?? nodes[0];
  const lines = [];
  let size = 0;
  let truncated = false;

  const emit = (depth, text) => {
    if (truncated) return;
    const line = `${'  '.repeat(depth)}- ${text}`;
    size += line.length + 1;
    if (size > max) {
      truncated = true;
      return;
    }
    lines.push(line);
  };

  const walk = (node, depth, parentSaid) => {
    if (!node || truncated) return;
    const kids = () => (node.childIds ?? []).map((id) => byId.get(id));
    const role = node.role?.value ?? '';
    const name = (node.name?.value ?? '').replace(/\s+/g, ' ').trim();
    if (node.ignored || (TRANSPARENT.has(role) && !name)) {
      for (const k of kids()) walk(k, depth, parentSaid);
      return;
    }
    if (role === 'StaticText') {
      // Text its parent already said, as name or value ("link Sign in" → "Sign in"), is noise.
      if (name && !parentSaid.includes(name)) emit(depth, JSON.stringify(name));
      return;
    }
    let text = role === 'RootWebArea' ? 'document' : role;
    if (name) text += ` ${JSON.stringify(name.length > 200 ? name.slice(0, 200) + '…' : name)}`;
    const value = node.value?.value;
    if (value !== undefined && value !== '' && value !== name) text += ` value=${JSON.stringify(String(value).slice(0, 200))}`;
    const level = prop(node, 'level');
    if (level) text += ` level=${level}`;
    for (const p of STATE_PROPS) {
      const v = prop(node, p);
      if (v === true || v === 'true' || v === 'mixed') text += v === 'mixed' ? ` ${p}=mixed` : ` ${p}`;
    }
    const focusable = prop(node, 'focusable');
    if (node.backendDOMNodeId && (INTERACTIVE.has(role) || focusable)) text += ` [ref=e${node.backendDOMNodeId}]`;
    emit(depth, text);
    for (const k of kids()) walk(k, depth + 1, `${name}\n${value ?? ''}`);
  };

  walk(root, 0, '');
  const info = await tabInfo(tabId);
  let text = lines.join('\n');
  if (truncated) text += `\n… (cut at ${max} characters; pass a larger max_chars for more)`;
  return { ...info, text, truncated };
}

// ── input ───────────────────────────────────────────────────────────────────

async function click(tabId, args) {
  const backendNodeId = refToNodeId(args.ref);
  try {
    await send(tabId, 'DOM.scrollIntoViewIfNeeded', { backendNodeId }).catch(() => {});
    let quads = [];
    try {
      ({ quads } = await send(tabId, 'DOM.getContentQuads', { backendNodeId }));
    } catch {
      quads = [];
    }
    if (quads?.length) {
      const q = quads[0];
      const x = (q[0] + q[2] + q[4] + q[6]) / 4;
      const y = (q[1] + q[3] + q[5] + q[7]) / 4;
      await send(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
      const clicks = args.double ? [1, 2] : [1];
      for (const clickCount of clicks) {
        await send(tabId, 'Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount });
        await send(tabId, 'Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount });
      }
    } else {
      // No box (zero-size, or off in a closed details): the DOM's own click.
      const objectId = await objectIdFor(tabId, backendNodeId);
      await send(tabId, 'Runtime.callFunctionOn', { objectId, functionDeclaration: 'function(){this.click()}' });
    }
  } catch (e) {
    throw staleRef(e);
  }
  return { clicked: true };
}

const KEYS = {
  Enter: { code: 'Enter', keyCode: 13, text: '\r' },
  Tab: { code: 'Tab', keyCode: 9 },
  Escape: { code: 'Escape', keyCode: 27 },
  Backspace: { code: 'Backspace', keyCode: 8 },
  Delete: { code: 'Delete', keyCode: 46 },
  ArrowLeft: { code: 'ArrowLeft', keyCode: 37 },
  ArrowUp: { code: 'ArrowUp', keyCode: 38 },
  ArrowRight: { code: 'ArrowRight', keyCode: 39 },
  ArrowDown: { code: 'ArrowDown', keyCode: 40 },
  Home: { code: 'Home', keyCode: 36 },
  End: { code: 'End', keyCode: 35 },
  PageUp: { code: 'PageUp', keyCode: 33 },
  PageDown: { code: 'PageDown', keyCode: 34 },
  Space: { code: 'Space', keyCode: 32, text: ' ', key: ' ' },
};

// CDP modifier bits. "Mod" is Cmd on a Mac and Ctrl elsewhere, so an agent needn't know which.
const MODIFIERS = { Alt: 1, Ctrl: 2, Control: 2, Meta: 4, Cmd: 4, Shift: 8 };
const IS_MAC = /Mac/i.test(navigator.userAgentData?.platform ?? navigator.platform ?? '');
// A synthetic Cmd/Ctrl+letter is only a key event: the browser's editing command behind it (select
// all, undo…) never runs unless it is named.
const EDIT_COMMANDS = { a: 'selectAll', z: 'undo', x: 'cut', c: 'copy', v: 'paste' };

/** A letter or digit has its own key code. Punctuation's character code is ANOTHER key's code
 *  ("." is 46, Delete; "(" is 40, ArrowDown), which Chrome runs instead of typing it, so it is sent
 *  as key code 0 and only its text is typed. */
function singleChar(ch) {
  if (/[a-z]/i.test(ch)) return { code: `Key${ch.toUpperCase()}`, keyCode: ch.toUpperCase().charCodeAt(0), text: ch };
  if (/[0-9]/.test(ch)) return { code: `Digit${ch}`, keyCode: ch.charCodeAt(0), text: ch };
  return { code: '', keyCode: 0, text: ch };
}

/** One key, or a combination like Shift+Tab or Mod+A. Goes to whatever has focus, in any frame. */
async function press(tabId, combo) {
  const parts = String(combo).split('+');
  const key = parts.length > 1 && parts.at(-1) === '' ? '+' : parts.pop(); // "Mod++" is Mod and "+"
  let modifiers = 0;
  for (const m of parts.filter(Boolean)) {
    const bit = m === 'Mod' ? (IS_MAC ? 4 : 2) : MODIFIERS[m];
    if (!bit) throw new Error(`Unknown modifier "${m}" in "${combo}". Use Shift, Alt, Ctrl, Meta (Cmd) or Mod (Cmd on a Mac, Ctrl elsewhere).`);
    modifiers |= bit;
  }
  const k = KEYS[key] ?? (key?.length === 1 ? singleChar(key) : null);
  if (!k) throw new Error(`Unknown key "${key}". Use Enter, Tab, Escape, Backspace, Delete, an arrow key, Home, End, PageUp, PageDown, Space or one character, optionally after modifiers (Shift+Tab, Mod+A).`);
  // Mod alone runs the editing command; Mod+Shift+Z is redo, and any other Shift or Alt combination
  // is left to the page (Mod+Shift+V is not a plain paste).
  const mod = IS_MAC ? 4 : 2;
  const lower = key.toLowerCase();
  const command =
    modifiers === mod ? EDIT_COMMANDS[lower] : modifiers === (mod | 8) && lower === 'z' ? 'redo' : undefined;
  // With Ctrl, Alt or Cmd held a key types nothing.
  const text = modifiers & 7 ? undefined : k.text;
  const base = { key: k.key ?? key, code: k.code, windowsVirtualKeyCode: k.keyCode, nativeVirtualKeyCode: k.keyCode, modifiers };
  await send(tabId, 'Input.dispatchKeyEvent', { type: text ? 'keyDown' : 'rawKeyDown', ...base, ...(text ? { text } : {}), ...(command ? { commands: [command] } : {}) });
  await send(tabId, 'Input.dispatchKeyEvent', { type: 'keyUp', ...base });
}

/** The element that really has focus: through open shadow roots and same-origin frames (into an
 *  editable body too: a rich-text editor's frame keeps focus on its body). Stops at a cross-origin
 *  frame or a closed shadow root, which the page can't see into. */
const DEEPEST_FOCUS = `function () {
  let el = this;
  for (;;) {
    const inner = el.shadowRoot ? el.shadowRoot.activeElement
      : (el.tagName === 'IFRAME' || el.tagName === 'FRAME') ? (() => { try { const d = el.contentDocument; return d && (d.activeElement !== d.body || d.body?.isContentEditable) ? d.activeElement : null; } catch { return null; } })()
      : null;
    if (!inner) return el;
    el = inner;
  }
}`;

/** Select an editable's whole contents, so typing replaces them (works with React-style inputs). */
const SELECT_ALL = `function () {
  if (typeof this.select === 'function' && ('value' in this)) { this.select(); return true; }
  if (this.isContentEditable) { const r = this.ownerDocument.createRange(); r.selectNodeContents(this);
    const s = this.ownerDocument.getSelection(); s.removeAllRanges(); s.addRange(r); return true; }
  return false;
}`;

/** Focus is past what the page can reach: a cross-origin frame, or a closed shadow root (which only
 *  the debugger can see). Only there is select-all-by-keyboard the way to clear the field. */
async function focusIsOutOfReach(tabId, objectId) {
  const { node } = await send(tabId, 'DOM.describeNode', { objectId, depth: 1, pierce: true });
  if (node.nodeName === 'IFRAME' || node.nodeName === 'FRAME') {
    // A frame the walk could read but stopped at has nothing focused inside: typing goes nowhere.
    const { result } = await send(tabId, 'Runtime.callFunctionOn', {
      objectId,
      functionDeclaration: 'function () { try { return !this.contentDocument; } catch { return true; } }',
      returnByValue: true,
    });
    return result.value === true;
  }
  return (node.shadowRoots ?? []).some((r) => r.shadowRootType === 'closed');
}

async function type(tabId, args) {
  const text = String(args.text ?? '');
  let objectId;
  try {
    if (args.ref) {
      const backendNodeId = refToNodeId(args.ref);
      await send(tabId, 'DOM.scrollIntoViewIfNeeded', { backendNodeId }).catch(() => {});
      await send(tabId, 'DOM.focus', { backendNodeId });
      objectId = await objectIdFor(tabId, backendNodeId);
    } else {
      const { result } = await send(tabId, 'Runtime.evaluate', { expression: 'document.activeElement' });
      objectId = result.objectId;
    }
  } catch (e) {
    throw staleRef(e);
  }
  let notCleared = false;
  if (args.clear !== false && objectId) {
    const deepest = (await send(tabId, 'Runtime.callFunctionOn', { objectId, functionDeclaration: DEEPEST_FOCUS })).result.objectId ?? objectId;
    const { result } = await send(tabId, 'Runtime.callFunctionOn', { objectId: deepest, functionDeclaration: SELECT_ALL, returnByValue: true });
    let selected = result.value === true;
    // Out of the page's reach: select all with the keyboard, which goes to whatever has focus.
    // Only then: on a list row or a button, Mod+A could select every item and Delete remove them.
    if (!selected && (await focusIsOutOfReach(tabId, deepest).catch(() => false))) {
      await press(tabId, 'Mod+A');
      selected = true;
    }
    if (!selected) notCleared = true;
    else if (!text) await press(tabId, 'Delete');
  }
  if (text) await send(tabId, 'Input.insertText', { text });
  if (args.submit) await press(tabId, 'Enter');
  return {
    typed: true,
    ...(notCleared ? { note: "What has focus isn't a text field, so nothing was cleared, and the text may have gone nowhere: take a snapshot to check." } : {}),
    ...(await tabInfo(tabId)),
  };
}

// ── the rest ────────────────────────────────────────────────────────────────

async function navigate(tabId, args) {
  const target = String(args.url ?? '').trim();
  if (!target) throw new Error('url is required');
  if (target === 'back') await chrome.tabs.goBack(tabId);
  else if (target === 'forward') await chrome.tabs.goForward(tabId);
  else if (target === 'reload') await chrome.tabs.reload(tabId);
  else {
    // A scheme, but not a port: `localhost:3000/x` is a host (tools.rs `has_scheme` agrees). With
    // none, https, except a local dev server, which is http as in Chrome's address bar.
    const local = /^(localhost|127(\.\d+){3}|\[::1\])([:/?#]|$)/i.test(target);
    const url = /^[a-z][a-z0-9+.-]*:(?!\d)/i.test(target) ? target : `${local ? 'http' : 'https'}://${target}`;
    await chrome.tabs.update(tabId, { url });
  }
  return { loaded: await waitForLoad(tabId) };
}

async function screenshot(tabId, args) {
  const params = { format: 'png', fromSurface: true };
  if (args.full_page) {
    const m = await send(tabId, 'Page.getLayoutMetrics');
    const size = m.cssContentSize ?? m.contentSize;
    params.captureBeyondViewport = true;
    params.format = 'jpeg';
    params.quality = 80;
    params.clip = { x: 0, y: 0, width: Math.ceil(size.width), height: Math.min(Math.ceil(size.height), 10000), scale: 1 };
  }
  const { data } = await send(tabId, 'Page.captureScreenshot', params);
  return { data, mimeType: params.format === 'png' ? 'image/png' : 'image/jpeg' };
}

async function evaluate(tabId, args) {
  const r = await send(tabId, 'Runtime.evaluate', {
    expression: String(args.expression ?? ''),
    returnByValue: true,
    awaitPromise: true,
    userGesture: true,
  });
  if (r.exceptionDetails) {
    throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text ?? 'The expression threw');
  }
  return { type: r.result.type, value: r.result.value ?? r.result.description ?? null };
}

const ERROR_LEVELS = new Set(['error', 'exception', 'assert']);

function consoleRead(tabId, args) {
  const buf = consoleBuf.get(tabId) ?? [];
  let messages = buf.slice();
  if (args.errors_only) messages = messages.filter((m) => ERROR_LEVELS.has(m.level));
  if (args.clear) {
    buf.length = 0;
    // A later re-attach replays the page's messages; the cleared ones must stay cleared.
    consoleLast.set(tabId, Date.now());
  }
  return { messages, note: messages.length ? undefined : 'Nothing logged since maiTerm attached. Reload the page (browserNavigate "reload") to capture what it logs while loading.' };
}

async function network(tabId, args) {
  const log = netLog.get(tabId) ?? new Map();
  if (args.request_id) {
    const r = log.get(String(args.request_id));
    if (!r) throw new Error('No such request in the log: list them with browserNetwork first.');
    const { body, base64Encoded } = await send(tabId, 'Network.getResponseBody', { requestId: r.id }).catch((e) => {
      throw new Error(`The browser no longer holds that response body (${e?.message ?? e}).`);
    });
    const text = base64Encoded ? `(binary, ${body.length} base64 characters)` : body;
    return { ...strip(r), body: text.length > BODY_MAX ? `${text.slice(0, BODY_MAX)}… (${text.length} characters)` : text };
  }
  const q = String(args.filter ?? '').toLowerCase();
  let rows = [...log.values()];
  if (args.failed_only) rows = rows.filter((r) => r.failed || r.status >= 400);
  if (q) rows = rows.filter((r) => r.url.toLowerCase().includes(q));
  if (!args.include_static) rows = rows.filter((r) => r.failed || r.status >= 400 || !['Image', 'Font', 'Stylesheet', 'Media'].includes(r.type));
  if (args.clear) log.clear();
  return {
    requests: rows.slice(-100).map(strip),
    note: rows.length > 100 ? `${rows.length - 100} older requests not shown` : rows.length ? undefined : 'No requests since maiTerm attached. Reload the page (browserNavigate "reload") to capture its loading requests.',
  };
}

function strip(r) {
  const { t0, ...rest } = r;
  return rest;
}

/** Run one action on a linked tab. Pages Chrome won't let an extension debug fail clearly. */
/** `onStart`: called once the debugger is attached, just before the action itself, which is when
 *  Chrome's 5 s window for a page to open a tab begins (background.js counts joins from it). */
export async function runAction(tabId, action, args = {}, onStart = () => {}) {
  // maiTerm sent a command for it, so it is linked (again).
  released.delete(tabId);
  const { url } = await tabInfo(tabId);
  if (/^(chrome|edge|brave|about|devtools|chrome-extension):/i.test(url) || /chromewebstore\.google\.com|chrome\.google\.com\/webstore/.test(url)) {
    if (action !== 'navigate') {
      throw new Error(`The browser doesn't let extensions work on ${url.split('/').slice(0, 3).join('/')} pages. Navigate somewhere else first.`);
    }
    // Navigating away is done through chrome.tabs, which needs no debugger.
    return { ...(await navigate(tabId, args)), ...(await tabInfo(tabId)) };
  }
  try {
    return await perform(tabId, action, args, onStart);
  } catch (e) {
    // Mid-command ("Detached while handling command") or between two commands ("Debugger is not
    // attached to the tab"): the same thing to the agent.
    if (!/detached while handling command|debugger is not attached/i.test(String(e?.message ?? e))) throw e;
    // Pushed out mid-command (another extension's frame appeared). A read is safe to repeat; an
    // action may or may not have happened, so the agent is told to look before repeating it.
    attached.delete(tabId);
    if (READS.has(action)) return perform(tabId, action, args, onStart);
    throw new Error(
      `Chrome detached maiTerm from the tab during that ${action} (another extension's frame, usually a password manager's popup, appeared). ` +
        'It may or may not have happened: take a snapshot before repeating it.',
    );
  }
}

/** Actions that change nothing, so repeating one after a detach is harmless. */
const READS = new Set(['snapshot', 'screenshot', 'console', 'network', 'wait']);

/** An action that can change the page, then the wait for it to settle (navigated, settled,
 *  waited_ms), then where the tab is now. This is what spares the agent sleeping between steps. */
async function settled(tabId, act) {
  const since = Date.now();
  const r = await act();
  const s = await settle(tabId, since, SETTLE_MAX_MS, Date.now());
  // Both may have something to say (type: nothing was cleared; settle: the page never went quiet).
  const note = [r.note, s.note].filter(Boolean).join(' ') || undefined;
  return { ...r, ...s, ...(note ? { note } : {}), ...(await tabInfo(tabId)) };
}

async function perform(tabId, action, args, onStart = () => {}) {
  await ensureAttached(tabId);
  onStart();
  switch (action) {
    case 'navigate':
      return settled(tabId, () => navigate(tabId, args));
    case 'snapshot':
      return snapshot(tabId, args);
    case 'click':
      return settled(tabId, () => click(tabId, args));
    case 'type':
      return settled(tabId, () => type(tabId, args));
    case 'press':
      return settled(tabId, async () => {
        await press(tabId, String(args.key ?? ''));
        return { pressed: true };
      });
    case 'wait':
      return waitFor(tabId, args);
    case 'screenshot':
      return screenshot(tabId, args);
    case 'eval':
      return evaluate(tabId, args);
    case 'console':
      return consoleRead(tabId, args);
    case 'network':
      return network(tabId, args);
    default:
      throw new Error(`Unknown action ${action}`);
  }
}
