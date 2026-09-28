/** Markdown for transcript turns in the Focus view.
 *
 *  A transcript is not trusted text. An agent quotes web pages, tool output and files, and this
 *  renders into the app's own webview, which can reach every Tauri command. So this instance
 *  never emits HTML it didn't build: raw HTML is shown as text, links are kept only for
 *  http(s) and mailto, and images become their alt text (no remote loads). */

import { Marked, Renderer, type Tokens } from 'marked';

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}

const SAFE_HREF = /^(https?:|mailto:)/i;

const renderer = new Renderer();
renderer.html = ({ text }: Tokens.HTML | Tokens.Tag) => escapeHtml(text);
renderer.image = ({ text }: Tokens.Image) => escapeHtml(text || 'image');
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
