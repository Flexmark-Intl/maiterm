// The popup: the ONE place a link is made (docs/browser-link.md §1.1) — the human, on the Chrome
// tab being linked, picks the maiTerm agent tab that may drive it.

const $ = (id) => document.getElementById(id);
const ask = (msg) => chrome.runtime.sendMessage(msg);

const RUNTIME = { claude: 'Claude', codex: 'Codex', gemini: 'Gemini' };

let chromeTab;
let groups = [];

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
  showError('');
  [chromeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const state = await ask({ type: 'state' });
  const known = state.instances.filter((i) => i.status !== 'not_installed');

  $('instances').replaceChildren(...known.map(instanceLine));
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
        el('span', { class: 'hint' }, 'That agent can diagnose this tab (console, network) and drive it (read, click, type). Chrome shows a debugging bar while it works; cancelling the bar also unlinks.'),
        el('button', { class: 'primary', onclick: unlink }, 'Unlink'),
      ),
    );
    $('picker').hidden = true;
    return;
  }

  $('current').replaceChildren(
    el('p', { class: 'hint' }, 'Choose the maiTerm agent tab that may diagnose and drive this page. It will act in this tab only, signed in as you.'),
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

function drawGroups() {
  const q = $('filter').value.trim().toLowerCase();
  const multi = groups.length > 1;
  const out = [];
  for (const g of groups) {
    const byWs = new Map();
    for (const t of g.tabs) {
      const hay = `${t.name} ${t.workspace} ${t.window}`.toLowerCase();
      if (q && !hay.includes(q)) continue;
      if (!byWs.has(t.workspace)) byWs.set(t.workspace, []);
      byWs.get(t.workspace).push(t);
    }
    for (const [ws, tabs] of byWs) {
      out.push(
        el(
          'div',
          { class: 'group' },
          el('h2', {}, multi ? `${g.instance} · ${ws}` : ws),
          ...tabs.map((t) =>
            el(
              'button',
              { class: 'tab', onclick: () => doLink(g.host, t) },
              el('span', { class: 'name' }, t.name),
              el(
                'span',
                { class: 'meta' },
                [RUNTIME[t.runtime] ?? t.runtime, t.linked ? `now linked to “${t.linked.title || 'another tab'}”` : null].filter(Boolean).join(' · '),
              ),
            ),
          ),
        ),
      );
    }
    if (g.error) out.push(el('p', { class: 'error' }, `${g.instance}: ${g.error}`));
  }
  if (!out.length) out.push(el('p', { class: 'hint' }, q ? 'No agent tab matches.' : 'No agent tabs are open in maiTerm.'));
  $('groups').replaceChildren(...out);
}

async function doLink(host, maitermTab) {
  const r = await ask({ type: 'link', host, maitermTab: { id: maitermTab.id, name: maitermTab.name }, chromeTab: { id: chromeTab.id, title: chromeTab.title, url: chromeTab.url } });
  if (r?.error) return showError(r.error);
  await render();
}

async function unlink() {
  const r = await ask({ type: 'unlink', chromeTabId: chromeTab.id });
  if (r?.error) return showError(r.error);
  await render();
}

$('filter').addEventListener('input', drawGroups);
render().catch((e) => showError(String(e?.message ?? e)));
