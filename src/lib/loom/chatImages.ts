/** Images an agent shows by path in the Focus chat (the phone's maiLink 0.18, on the desktop).
 *
 *  `renderTurnMarkdown` turns an image into a `span.md-img[data-src]` placeholder that loads
 *  nothing. This action finds those placeholders and asks Rust (`get_chat_image`) for the file:
 *  Rust serves it only if it is a path the agent itself wrote, an image, on the computer the agent
 *  ran on — the same reader the phone uses, so the two can't disagree. A URL (http, data…) is never
 *  fetched; it stays its alt text, as before.
 *
 *  The chat re-renders on every poll, so results are cached per tab and path: a shown image for
 *  five minutes (an agent that regenerates a screenshot gets the new one after that), a refusal
 *  for one, so a re-render doesn't ask again. */

import { getChatImage, type ChatImage } from '$lib/tauri/commands';

const OK_TTL_MS = 5 * 60_000;
const REFUSED_TTL_MS = 60_000;
/** Data URLs can be large; keep only the most recent few. */
const MAX_CACHED = 40;

const cache = new Map<string, { expires: number; result: Promise<ChatImage> }>();

/** A file path the agent wrote — not a URL with any other scheme. */
function isFilePath(src: string): boolean {
  return src.startsWith('file:') || !/^[a-z][a-z0-9+.-]*:/i.test(src);
}

function load(tabId: string, path: string): Promise<ChatImage> {
  const key = `${tabId}\u0000${path}`;
  const hit = cache.get(key);
  if (hit && Date.now() < hit.expires) {
    cache.delete(key);
    cache.set(key, hit); // most recently used last
    return hit.result;
  }
  const result = getChatImage(tabId, path).catch((e) => ({ ok: false as const, reason: String(e) }));
  // Held while in flight; its lifetime is set by how it ended.
  const entry = { expires: Number.POSITIVE_INFINITY, result };
  void result.then((r) => (entry.expires = Date.now() + (r.ok ? OK_TTL_MS : REFUSED_TTL_MS)));
  cache.delete(key);
  cache.set(key, entry);
  while (cache.size > MAX_CACHED) cache.delete(cache.keys().next().value!);
  return result;
}

function hydrate(el: HTMLElement, tabId: string) {
  if (el.dataset.state) return;
  const raw = el.dataset.src ?? '';
  // The Markdown renderer percent-encodes a src; the agent wrote it decoded (Rust accepts both).
  let path = raw;
  try {
    path = decodeURI(raw);
  } catch {
    /* keep it raw */
  }
  if (!path || !isFilePath(path)) {
    el.dataset.state = 'skipped';
    return;
  }
  el.dataset.state = 'loading';
  const alt = el.textContent ?? '';
  void load(tabId, path).then((r) => {
    if (!el.isConnected) return;
    if (r.ok) {
      const img = document.createElement('img');
      img.className = 'md-img-loaded';
      img.src = r.url;
      img.alt = alt;
      el.replaceWith(img);
    } else {
      el.dataset.state = 'refused';
      el.textContent = `${alt} — ${r.reason}`;
    }
  });
}

/** `use:chatImages={tabId}` on an element holding rendered agent Markdown. */
export function chatImages(node: HTMLElement, tabId: string) {
  let tab = tabId;
  const run = () => node.querySelectorAll<HTMLElement>('span.md-img[data-src]').forEach((el) => hydrate(el, tab));
  // `{@html}` replaces the nodes whenever the text changes, without telling an action.
  const observer = new MutationObserver(run);
  observer.observe(node, { childList: true, subtree: true });
  run();
  return {
    update(next: string) {
      tab = next;
      run();
    },
    destroy() {
      observer.disconnect();
    },
  };
}
