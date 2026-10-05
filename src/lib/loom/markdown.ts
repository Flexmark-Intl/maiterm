/** Markdown for transcript turns in the Focus view.
 *
 *  A transcript is not trusted text. An agent quotes web pages, tool output and files, and this
 *  renders into the app's own webview, which can reach every Tauri command. So this instance
 *  never emits HTML it didn't build: raw HTML is shown as text, links are kept only for
 *  http(s) and mailto, and an image becomes a placeholder holding its alt text and its src — it
 *  loads nothing. `chatImages` (loom/chatImages.ts) then asks Rust for the image, which serves
 *  only a local or remote FILE path the agent itself wrote (`get_chat_image`), never a URL. */

import { Marked, Renderer, type Tokens } from 'marked';

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}

const SAFE_HREF = /^(https?:|mailto:)/i;

const renderer = new Renderer();
renderer.html = ({ text }: Tokens.HTML | Tokens.Tag) => escapeHtml(text);
renderer.image = ({ href, text }: Tokens.Image) =>
  `<span class="md-img" data-src="${escapeHtml(href ?? '')}">${escapeHtml(text || 'image')}</span>`;
renderer.link = function (this: Renderer, { href, title, tokens }: Tokens.Link) {
  const inner = this.parser.parseInline(tokens);
  if (!SAFE_HREF.test(href.trim())) return inner;
  const t = title ? ` title="${escapeHtml(title)}"` : '';
  return `<a href="${escapeHtml(href)}"${t} target="_blank" rel="noopener noreferrer">${inner}</a>`;
};

const md = new Marked({ gfm: true, breaks: true, async: false, renderer });

export function renderTurnMarkdown(text: string): string {
  return md.parse(text) as string;
}
