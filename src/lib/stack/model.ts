/** Pure helpers for the workspace stack (docs/stack.md) — no Svelte, no Tauri, so they
 *  are unit-testable. The store in `stores/stack.svelte.ts` wires them to live state. */

import type { Service } from '$lib/tauri/types';

export type ServiceStatus = 'stopped' | 'starting' | 'running' | 'ready' | 'crashed';

/** Restart-on-crash backoff and ceiling (docs/stack.md §5). */
export const RESTART_BACKOFF_MS = [1000, 2000, 4000, 8000, 16000, 30000];
export const RESTART_CEILING = 5;
export const RESTART_WINDOW_MS = 10 * 60 * 1000;

/** POSIX single-quote. Works in sh/bash/zsh and fish. */
export function shq(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

/** The line typed into the service tab. `env` prefix rather than `K=V cmd` so fish
 *  works too; `cd` first so a relative command resolves where the human expects. */
export function startLine(service: Pick<Service, 'cwd' | 'env' | 'command'>): string {
  const parts: string[] = [];
  if (service.cwd) parts.push(`cd ${shq(service.cwd)}`);
  const env = service.env ?? [];
  const cmd = env.length
    ? `env ${env.map(([k, v]) => `${k}=${shq(v)}`).join(' ')} ${service.command}`
    : service.command;
  parts.push(cmd);
  return parts.join(' && ');
}

/** SIGINT (130) and a clean exit both mean "it stopped"; anything else crashed. */
export function exitIsCrash(code: number): boolean {
  return code !== 0 && code !== 130;
}

/** Whether another automatic restart is allowed: fewer than RESTART_CEILING inside the
 *  window. Pure, so it is testable without a clock. */
export function restartAllowed(restarts: number[], now: number): boolean {
  return restarts.filter((t) => now - t < RESTART_WINDOW_MS).length < RESTART_CEILING;
}

/** Delay before the next automatic restart, growing with the recent count. */
export function restartDelay(recentRestarts: number): number {
  return RESTART_BACKOFF_MS[Math.min(recentRestarts, RESTART_BACKOFF_MS.length - 1)];
}

// ── Endpoint detection (docs/stack.md §9) ──────────────────────────────────────────
//
// maiTerm reads a service's own output for the address it is serving on, so a port
// reaches the sidebar without an agent in the loop. Deliberately NOT socket sniffing:
// the PID is known, but asking the OS means a subprocess on a timer, permissions, and
// a process that opens three ports and a helper that opens two more.

/** Hosts a server prints to mean "every interface", which a browser cannot follow.
 *  Rewritten so the stored URL is one a human can actually click. */
const BROWSABLE: Record<string, string> = {
  '0.0.0.0': 'localhost',
  '[::]': 'localhost',
  '[::1]': 'localhost',
};

/** The host is restricted to loopback ON PURPOSE: a start-up banner routinely also
 *  carries a docs link or a network address, and only the loopback one is the thing
 *  this service is serving. */
const LOOPBACK = String.raw`localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|\[::\]`;

/** `http://localhost:5173/`, `➜  Local:   https://127.0.0.1:8443/` — what nearly every
 *  dev server prints. ANSI is stripped before this runs. */
const URL_RE = new RegExp(String.raw`\b(https?)://(${LOOPBACK})(?::(\d{1,5}))?`, 'i');

/** A line naming a port it did NOT get. Vite prints `Port 5173 is in use, trying another
 *  one...` and Next `⚠ Port 3000 is in use, trying 3001 instead.` — both BEFORE binding the
 *  port they actually take, so believing the first number in the output records the wrong
 *  one and then disarms the scan that would have found the right one. */
const REJECT_RE = /\b(?:in use|already|EADDRINUSE|unavailable|failed|failure|error|cannot|could not|couldn't|can't|retry|retrying|trying|instead)\b/i;

/** A serving verb is REQUIRED before a bare port. Without it `--port 5173` in a command
 *  line matches — and the pty echoes back the very line maiTerm typed to start the
 *  service, so the scan would otherwise match the start line against itself before the
 *  process has bound anything. `\bport\b` will not match inside "support". */
const PORT_WORD_RE = /\b(?:listening|serving|bound|available|accessible)\b[^\n]{0,40}?\bport\b\s*[:=]?\s*(\d{2,5})\b/i;

/** `listening on 0.0.0.0:4000`, `serving at :3000`. Same rule, same reason — and a bare
 *  `:3000` also matches a duration ("in 3:42") and a great deal else besides. */
const HOST_PORT_RE = /\b(?:listening|serving|bound)\b\s*(?:on|at)?\s*(?:[a-z0-9.\-]*|\[[0-9a-f:]*\]):(\d{2,5})\b/i;

export interface DetectedEndpoint {
  port: number;
  /** Only when the service announced a scheme — a bare port is not launchable. */
  url: string | null;
}

function validPort(n: number): boolean {
  return Number.isInteger(n) && n > 0 && n <= 65535;
}

function fromUrl(line: string): DetectedEndpoint | null {
  const m = URL_RE.exec(line);
  if (!m) return null;
  const scheme = m[1].toLowerCase();
  const host = BROWSABLE[m[2].toLowerCase()] ?? m[2].toLowerCase();
  const port = m[3] ? Number(m[3]) : scheme === 'https' ? 443 : 80;
  if (!validPort(port)) return null;
  const implicit = port === (scheme === 'https' ? 443 : 80);
  return { port, url: implicit ? `${scheme}://${host}` : `${scheme}://${host}:${port}` };
}

/** The endpoint a chunk of (ANSI-stripped) service output announces, or null.
 *
 *  Line by line, because a conflict warning and the real address routinely arrive in one
 *  chunk and only the line carrying the number decides whether to believe it. Two passes,
 *  so a URL anywhere in the chunk beats a bare port anywhere else: the URL carries the
 *  scheme, which is what makes it openable. */
export function detectEndpoint(text: string): DetectedEndpoint | null {
  const lines = text.split('\n').filter((l) => !REJECT_RE.test(l));
  for (const line of lines) {
    const hit = fromUrl(line);
    if (hit) return hit;
  }
  for (const line of lines) {
    for (const re of [PORT_WORD_RE, HOST_PORT_RE]) {
      const p = re.exec(line);
      if (p) {
        const port = Number(p[1]);
        if (validPort(port)) return { port, url: null };
      }
    }
  }
  return null;
}

/** Whether an endpoint is something "Open in browser" can actually open. */
export function launchableUrl(service: Pick<Service, 'url'>): string | null {
  const url = service.url ?? null;
  return url && /^https?:\/\//i.test(url) ? url : null;
}

export type Rollup = 'ready' | 'starting' | 'partial' | 'crashed' | null;

/** Rollup for the sidebar dot, batch semantics like the Claude indicator (docs/stack.md
 *  §7): red if anything crashed, amber while anything starts, green only when EVERY
 *  auto-start service is up, `partial` when some are up and some never started, null when
 *  the stack is empty or fully stopped. */
export function rollupStatus(rows: { status: ServiceStatus; autoStart: boolean }[]): Rollup {
  if (rows.length === 0) return null;
  const up = (s: ServiceStatus) => s === 'running' || s === 'ready';
  if (rows.some((r) => r.status === 'crashed')) return 'crashed';
  if (rows.some((r) => r.status === 'starting')) return 'starting';
  const anyUp = rows.some((r) => up(r.status));
  if (!anyUp) return null;
  const expected = rows.filter((r) => r.autoStart);
  const allExpectedUp = expected.length > 0 ? expected.every((r) => up(r.status)) : rows.every((r) => up(r.status));
  return allExpectedUp ? 'ready' : 'partial';
}
