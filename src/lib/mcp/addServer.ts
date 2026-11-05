/** The human's MCP add path (docs/mcp-proxy.md §7), the parts every caller shares: Preferences →
 *  MCP servers, an approved agent `mcp_add`, and the Hive media setup sheet (docs/hive-media.md §6).
 *  Pure, apart from `startSignIn`, which opens the browser. */

import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { mcpServerSignIn } from '$lib/tauri/commands';

/** A server name maiTerm accepts (`mcp_proxy::valid_id`): 1–40 lowercase letters, digits, - or _. */
export const SERVER_NAME_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/;

/** A name from the URL's host: mcp.linear.app → linear, api.githubcopilot.com → githubcopilot, an
 *  IP or localhost → local. "" for a URL that doesn't parse. */
export function suggestServerName(raw: string): string {
  try {
    const host = new URL(raw).hostname;
    if (/^[\d.]+$/.test(host) || host === 'localhost') return 'local';
    const labels = host.split('.').filter((l) => !['mcp', 'api', 'www'].includes(l));
    const pick = labels.length > 1 ? labels[labels.length - 2] : labels[0] ?? '';
    return pick.toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/^-+/, '').slice(0, 40);
  } catch {
    return '';
  }
}

/** The name agents see a server under: a dev build suffixes it (`mcp_proxy::entries::entry_name`). */
export function entryName(name: string, dev: boolean): string {
  return dev ? `${name}-dev` : name;
}

/** One URL, as `mcp_proxy::same_url` compares: scheme, host and port case-insensitively, the path
 *  case-sensitively, a trailing slash not counting, the query exactly. */
export function sameServerUrl(a: string, b: string): boolean {
  try {
    const x = new URL(a.trim());
    const y = new URL(b.trim());
    return x.protocol === y.protocol && x.host === y.host && x.pathname.replace(/\/+$/, '') === y.pathname.replace(/\/+$/, '') && x.search === y.search;
  } catch {
    return a.trim().replace(/\/+$/, '') === b.trim().replace(/\/+$/, '');
  }
}

/** `base`, or `base-2`, `base-3`… : the first that no maiTerm server has as its id and no entry of
 *  the human's own holds as the name agents would see (a native entry is never written over, so the
 *  server would quietly not reach that runtime). Null when `base` isn't a valid name. */
export function freeServerName(base: string, takenIds: readonly string[], nativeNames: readonly string[], dev: boolean): string | null {
  if (!SERVER_NAME_RE.test(base)) return null;
  const free = (n: string) => !takenIds.includes(n) && !nativeNames.includes(entryName(n, dev));
  if (free(base)) return base;
  for (let i = 2; i < 100; i++) {
    const suffix = `-${i}`;
    const n = `${base.slice(0, 40 - suffix.length)}${suffix}`;
    if (free(n)) return n;
  }
  return null;
}

/** Start server `id`'s sign-in in the system browser. Only an http(s) URL is opened: maiTerm built
 *  it, but a browser handed any other scheme would launch whatever app claims it. Rejects with
 *  plain words. */
export async function startSignIn(id: string): Promise<void> {
  const url = await mcpServerSignIn(id);
  let ok = false;
  try {
    const u = new URL(url);
    ok = u.protocol === 'https:' || u.protocol === 'http:';
  } catch {
    ok = false;
  }
  if (!ok) throw new Error('the sign-in address maiTerm got is not a web page');
  await shellOpen(url);
}
