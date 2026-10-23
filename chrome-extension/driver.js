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
  attached.add(tabId);
  if (!consoleBuf.has(tabId)) consoleBuf.set(tabId, []);
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

export async function detach(tabId) {
  attached.delete(tabId);
  consoleBuf.delete(tabId);
  netLog.delete(tabId);
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
  if (keepLogs) return;
  consoleBuf.delete(tabId);
  netLog.delete(tabId);
}

function pushConsole(tabId, line) {
  const buf = consoleBuf.get(tabId);
  if (!buf) return;
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
  if (method.startsWith('Network.')) {
    onNetwork(tabId, method, params);
  } else if (method === 'Runtime.consoleAPICalled') {
    const text = (params.args ?? []).map(remoteObjectText).join(' ');
    pushConsole(tabId, { level: params.type, text, source: where(params.stackTrace), at: new Date(params.timestamp).toISOString() });
  } else if (method === 'Runtime.exceptionThrown') {
    const d = params.exceptionDetails ?? {};
    const text = d.exception?.description ?? d.text ?? 'Uncaught exception';
    pushConsole(tabId, { level: 'exception', text, source: where(d.stackTrace, d.url, d.lineNumber, d.columnNumber), at: new Date(params.timestamp).toISOString() });
  } else if (method === 'Log.entryAdded') {
    // Browser-side messages: CSP violations, failed resource loads, deprecations.
    const e = params.entry;
    pushConsole(tabId, { level: e.level, text: e.text, source: e.url, origin: e.source, at: new Date(e.timestamp).toISOString() });
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
  await sleep(300);
  return { clicked: true, ...(await tabInfo(tabId)) };
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

async function press(tabId, key) {
  const k = KEYS[key] ?? (key?.length === 1 ? { code: '', keyCode: key.toUpperCase().charCodeAt(0), text: key } : null);
  if (!k) throw new Error(`Unknown key "${key}". Use Enter, Tab, Escape, Backspace, Delete, an arrow key, Home, End, PageUp, PageDown, Space or one character.`);
  const base = { key: k.key ?? key, code: k.code, windowsVirtualKeyCode: k.keyCode, nativeVirtualKeyCode: k.keyCode };
  await send(tabId, 'Input.dispatchKeyEvent', { type: k.text ? 'keyDown' : 'rawKeyDown', ...base, ...(k.text ? { text: k.text } : {}) });
  await send(tabId, 'Input.dispatchKeyEvent', { type: 'keyUp', ...base });
}

/** Select an editable's whole contents, so typing replaces them (works with React-style inputs). */
const SELECT_ALL = `function () {
  if (typeof this.select === 'function' && ('value' in this)) { this.select(); return true; }
  if (this.isContentEditable) { const r = document.createRange(); r.selectNodeContents(this);
    const s = getSelection(); s.removeAllRanges(); s.addRange(r); return true; }
  return false;
}`;

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
  if (args.clear !== false && objectId) {
    await send(tabId, 'Runtime.callFunctionOn', { objectId, functionDeclaration: SELECT_ALL });
    if (!text) await press(tabId, 'Delete');
  }
  if (text) await send(tabId, 'Input.insertText', { text });
  if (args.submit) {
    await press(tabId, 'Enter');
    await sleep(300);
  }
  return { typed: true, ...(await tabInfo(tabId)) };
}

// ── the rest ────────────────────────────────────────────────────────────────

async function navigate(tabId, args) {
  const target = String(args.url ?? '').trim();
  if (!target) throw new Error('url is required');
  if (target === 'back') await chrome.tabs.goBack(tabId);
  else if (target === 'forward') await chrome.tabs.goForward(tabId);
  else if (target === 'reload') await chrome.tabs.reload(tabId);
  else {
    const url = /^[a-z][a-z0-9+.-]*:/i.test(target) ? target : `https://${target}`;
    await chrome.tabs.update(tabId, { url });
  }
  const loaded = await waitForLoad(tabId);
  return { loaded, ...(await tabInfo(tabId)) };
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
  if (args.clear) buf.length = 0;
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
export async function runAction(tabId, action, args = {}) {
  const { url } = await tabInfo(tabId);
  if (/^(chrome|edge|brave|about|devtools|chrome-extension):/i.test(url) || /chromewebstore\.google\.com|chrome\.google\.com\/webstore/.test(url)) {
    if (action !== 'navigate') {
      throw new Error(`The browser doesn't let extensions work on ${url.split('/').slice(0, 3).join('/')} pages. Navigate somewhere else first.`);
    }
    // Navigating away is done through chrome.tabs, which needs no debugger.
    return navigate(tabId, args);
  }
  try {
    return await perform(tabId, action, args);
  } catch (e) {
    if (!/detached while handling command/i.test(String(e?.message ?? e))) throw e;
    // Pushed out mid-command (another extension's frame appeared). A read is safe to repeat; an
    // action may or may not have happened, so the agent is told to look before repeating it.
    attached.delete(tabId);
    if (READS.has(action)) return perform(tabId, action, args);
    throw new Error(
      `Chrome detached maiTerm from the tab during that ${action} (another extension's frame, usually a password manager's popup, appeared). ` +
        'It may or may not have happened: take a snapshot before repeating it.',
    );
  }
}

/** Actions that change nothing, so repeating one after a detach is harmless. */
const READS = new Set(['snapshot', 'screenshot', 'console', 'network']);

async function perform(tabId, action, args) {
  await ensureAttached(tabId);
  switch (action) {
    case 'navigate':
      return navigate(tabId, args);
    case 'snapshot':
      return snapshot(tabId, args);
    case 'click':
      return click(tabId, args);
    case 'type':
      return type(tabId, args);
    case 'press':
      await press(tabId, String(args.key ?? ''));
      await sleep(150);
      return { pressed: true, ...(await tabInfo(tabId)) };
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
