// The popup: the ONE place the human links a tab (docs/browser-link.md §1.1) — on the Chrome tab
// being linked, they pick the maiTerm agent tab that may drive it. Agent tabs are listed most
// recently used first, each window's front tab on top (maiTerm orders them).

const $ = (id) => document.getElementById(id);
const ask = (msg) => chrome.runtime.sendMessage(msg);

const RUNTIME = { claude: 'Claude', codex: 'Codex', gemini: 'Gemini' };

let chromeTab;
let groups = [];
/** Just linked: the purpose box takes the keyboard, since writing one is the next step. */
let justLinked = false;
/** The purpose box on screen: its input, what was last sent, and its pending save. Dropped (the
 *  pending save cancelled) whenever the popup redraws, so a note typed for one link can't land
 *  on the tab after it is unlinked or linked elsewhere. */
let purposeBox = null;

function dropPurposeBox() {
  if (purposeBox) clearTimeout(purposeBox.timer);
  purposeBox = null;
}

// The popup closes the moment it loses focus: whatever wasn't sent yet goes now.
addEventListener('pagehide', () => {
  const b = purposeBox;
  if (!b) return;
  clearTimeout(b.timer);
  if (b.input.value.trim() !== b.sent.trim()) ask({ type: 'setPurpose', chromeTabId: b.chromeTabId, purpose: b.input.value });
});

function el(tag, attrs = {}, ...children) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') n.className = v;
    else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
    else n.setAttribute(k, v);
  }
  for (const c of children) if (c != null) n.append(c);
  return n;
}

function showError(text) {
  $('error').textContent = text;
  $('error').hidden = !text;
}

function instanceLine(i) {
  const label = i.instance ?? (i.host.endsWith('_dev') ? 'maiTerm (dev)' : 'maiTerm');
  const [cls, words] = {
    connected: ['connected', 'connected'],
    connecting: ['waiting', 'connecting…'],
    not_running: ['off', 'not running'],
    disconnected: ['waiting', 'reconnecting…'],
    refused: ['off', i.error ?? 'refused'],
  }[i.status] ?? ['off', i.status];
  return el('div', { class: 'instance' }, el('span', { class: `dot ${cls}` }), `${label} — ${words}`);
}

async function render() {
  dropPurposeBox();
  showError('');
  [chromeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const state = await ask({ type: 'state' });
  const known = state.instances.filter((i) => i.status !== 'not_installed');

  $('instances').replaceChildren(...known.map(instanceLine));
  drawLinked(state.links);
  if (!known.length) {
    $('current').replaceChildren(
      el('p', { class: 'hint' }, 'maiTerm isn’t set up for this browser yet. In maiTerm, open Preferences → Chrome extension and turn it on, then open this again.'),
    );
    $('picker').hidden = true;
    return;
  }

  const link = state.links.find((l) => l.chromeTabId === chromeTab?.id);
  if (link) {
    const where = state.instances.find((i) => i.host === link.host)?.instance ?? 'maiTerm';
    $('current').replaceChildren(
      el(
        'div',
        { class: 'card' },
        el('span', { class: 'label' }, `Linked · ${where}`),
        el('span', { class: 'name' }, link.maitermTabName ?? 'an agent tab'),
        link.handle
          ? el('span', { class: 'meta' }, [`The agent knows this tab as ${link.handle}`, link.openedBy ? `opened by ${link.openedBy}` : null].filter(Boolean).join(' · '))
          : null,
        purposeField(link),
        el('span', { class: 'hint' }, 'That agent can diagnose this tab (console, network) and drive it (read, click, type), and no other tab. While it does, Chrome shows its “debugging this browser” bar in every tab: that bar is Chrome’s and covers the whole browser. Cancelling it unlinks every linked tab.'),
        el('button', { class: 'primary', onclick: unlink }, 'Unlink'),
      ),
    );
    $('picker').hidden = true;
    if (justLinked) $('purpose').focus();
    justLinked = false;
    return;
  }

  $('current').replaceChildren(
    el('p', { class: 'hint' }, 'Choose the maiTerm agent tab that may diagnose and drive this page, signed in as you. An agent can hold several linked tabs; tabs this page opens for it (a sign-in popup) join it too.'),
  );
  if (!known.some((i) => i.status === 'connected')) {
    $('picker').hidden = true;
    return;
  }
  const r = await ask({ type: 'listTabs' });
  groups = r.groups ?? [];
  $('picker').hidden = false;
  drawGroups();
  $('filter').focus();
}

/** What this tab is for, in the human's words: the agent reads it to tell its tabs apart. Saved
 *  when the field is left or on Enter. */
function purposeField(link) {
  const status = el('span', { class: 'saved', 'aria-live': 'polite' });
  const input = el('input', {
    id: 'purpose',
    type: 'text',
    maxlength: '200',
    placeholder: 'e.g. staging admin, signed in as support',
    autocomplete: 'off',
    spellcheck: 'false',
  });
  input.value = link.purpose ?? '';
  const box = { input, sent: input.value, timer: undefined, chromeTabId: chromeTab.id };
  purposeBox = box;
  const save = async () => {
    if (purposeBox !== box) return;
    // What is sent now, not what the box holds when the answer comes back: they may type on.
    const sending = input.value;
    if (sending.trim() === box.sent.trim()) return;
    const before = box.sent;
    box.sent = sending;
    const r = await ask({ type: 'setPurpose', chromeTabId: box.chromeTabId, purpose: sending });
    if (purposeBox !== box) return;
    if (r?.error) {
      if (box.sent === sending) box.sent = before;
      return showError(r.error);
    }
    status.textContent = sending.trim() ? 'Saved: the agent is told' : 'Cleared';
  };
  // Saved as they type: the popup closes the moment it loses focus, before a change event.
  input.addEventListener('input', () => {
    clearTimeout(box.timer);
    box.timer = setTimeout(save, 500);
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      clearTimeout(box.timer);
      save();
    }
  });
  return el(
    'label',
    { class: 'purpose' },
    el('span', { class: 'label' }, 'What is this tab for?'),
    input,
    el('span', { class: 'hint' }, 'Optional. With several tabs linked, the agent uses this to load each page in the right one.'),
    status,
  );
}

/** Every linked tab, from any page: which ones they are, and a way to get to each. */
function drawLinked(links) {
  const others = links.filter((l) => l.chromeTabId !== chromeTab?.id);
  $('linked').hidden = !others.length;
  if (!others.length) return;
  $('linked').replaceChildren(
    el('h2', {}, 'Linked tabs'),
    ...others.map((l) =>
      el(
        'button',
        { class: 'tab', onclick: () => goTo(l.chromeTabId) },
        el('span', { class: 'name' }, l.title || l.url || 'A linked tab'),
        el('span', { class: 'meta' }, [l.maitermTabName ?? 'an agent tab', l.handle, l.purpose ? `for “${l.purpose}”` : null].filter(Boolean).join(' · ')),
      ),
    ),
  );
}

async function goTo(chromeTabId) {
  try {
    const t = await chrome.tabs.update(chromeTabId, { active: true });
    await chrome.windows.update(t.windowId, { focused: true });
    window.close();
  } catch (e) {
    showError(String(e?.message ?? e));
  }
}

function drawGroups() {
  const q = $('filter').value.trim().toLowerCase();
  const multi = groups.length > 1;
  const out = [];
  for (const g of groups) {
    const tabs = g.tabs.filter((t) => !q || `${t.name} ${t.workspace} ${t.window}`.toLowerCase().includes(q));
    if (!tabs.length && !g.error) continue;
    out.push(
      el(
        'div',
        { class: 'group' },
        multi ? el('h2', {}, g.instance) : null,
        ...tabs.map((t) => {
          const linked = t.linked ?? [];
          const meta = [
            RUNTIME[t.runtime] ?? t.runtime,
            g.windows > 1 ? `${t.workspace} · ${t.window}` : t.workspace,
            linked.length ? `${linked.length} linked tab${linked.length > 1 ? 's' : ''}` : null,
          ];
          return el(
            'button',
            { class: 'tab', onclick: () => doLink(g.host, t) },
            el('span', { class: 'name' }, t.name, t.front ? el('span', { class: 'front' }, 'in front') : null),
            el('span', { class: 'meta' }, meta.filter(Boolean).join(' · ')),
          );
        }),
      ),
    );
    if (g.error) out.push(el('p', { class: 'error' }, `${g.instance}: ${g.error}`));
  }
  if (!out.length) out.push(el('p', { class: 'hint' }, q ? 'No agent tab matches.' : 'No agent tabs are open in maiTerm.'));
  $('groups').replaceChildren(...out);
}

async function doLink(host, maitermTab) {
  const r = await ask({ type: 'link', host, maitermTab: { id: maitermTab.id, name: maitermTab.name }, chromeTab: { id: chromeTab.id, title: chromeTab.title, url: chromeTab.url } });
  if (r?.error) return showError(r.error);
  justLinked = true;
  await render();
}

async function unlink() {
  dropPurposeBox();
  const r = await ask({ type: 'unlink', chromeTabId: chromeTab.id });
  if (r?.error) return showError(r.error);
  await render();
}

$('filter').addEventListener('input', drawGroups);
render().catch((e) => showError(String(e?.message ?? e)));
