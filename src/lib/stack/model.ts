/** Pure helpers for the workspace stack (docs/stack.md) — no Svelte, no Tauri, so they
 *  are unit-testable. The store in `stores/stack.svelte.ts` wires them to live state. */

import type { Service } from '$lib/tauri/types';
import type { StackRuntimeRow } from '$lib/tauri/commands';
import { normalizeTitle } from '$lib/tasks/model';

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

// ── Copies: one service on several checkouts (docs/hive.md §6.9) ───────────────────
//
// A copy is `(service_id, target)`. The target is Rust's string (`stack/target.rs`): `''` is
// the service's own folder — the only copy a service had before copies existed — else
// `checkout:<abs>` or `branch:<name>`. Every runtime map in the store is keyed by the copy
// key, and the own folder's key IS the service id, so a legacy service keys exactly as before.

/** The most copies of one service that run at once (plan decision 7). */
export const MAX_COPIES = 3;

/** The store's (and Rust's `stack::copy_key`) key: the id for the own folder, else `id@target`. */
export function copyKey(serviceId: string, target = ''): string {
  return target ? `${serviceId}@${target}` : serviceId;
}

/** Ids are uuids, so the first `@` splits a key. */
export function parseCopyKey(key: string): { serviceId: string; target: string } {
  const at = key.indexOf('@');
  return at < 0 ? { serviceId: key, target: '' } : { serviceId: key.slice(0, at), target: key.slice(at + 1) };
}

/** What a copy is called before Rust has resolved it (a label it gave wins): the branch, the
 *  checkout's folder, `main` for the own folder. */
export function targetLabel(target: string): string {
  if (!target) return 'main';
  if (target.startsWith('branch:')) return target.slice('branch:'.length);
  if (target.startsWith('checkout:')) return target.slice('checkout:'.length).split('/').filter(Boolean).pop() ?? target;
  return target;
}

/** `web · on session-store@3f2a91c`, or plain `web` for the own folder (docs/hive.md §6.9). */
export function copyTitle(name: string, target: string, copy?: { label?: string | null; commit?: string | null } | null): string {
  if (!target) return name;
  const label = copy?.label || targetLabel(target);
  return `${name} · on ${label}${copy?.commit ? `@${copy.commit}` : ''}`;
}

/** The hidden tab a copy runs in: `web@session-store`; the own folder's is just `web`. */
export function copyTabName(name: string, target: string, label?: string | null): string {
  return target ? `${name}@${label || targetLabel(target)}` : name;
}

/** The placeholders in `text`, as Rust's `placeholders::scan` reads them: `{port}`, `{port2}`,
 *  `{branch}`, `{worker}`, `{checkout}`, `{port:<svc>}`; every other brace is left alone. */
export function placeholdersIn(text: string): string[] {
  const out: string[] = [];
  let i = 0;
  while (true) {
    const open = text.indexOf('{', i);
    if (open < 0) break;
    const rest = text.slice(open + 1);
    const j = rest.search(/[{}]/);
    if (j < 0) break;
    if (rest[j] === '}') {
      const inner = rest.slice(0, j);
      const named = ['port', 'port2', 'branch', 'worker', 'checkout'].includes(inner);
      const portOf = inner.startsWith('port:') && [...inner.slice(5)].length >= 1 && [...inner.slice(5)].length <= 64;
      if (named || portOf) {
        out.push(inner);
        i = open + j + 2;
        continue;
      }
    }
    i = open + 1;
  }
  return out;
}

/** `{port}` anywhere: each copy gets its own port, so several run at once. Without it the
 *  service runs ONE copy at a time (Rust's `Needs::one_copy`). */
export function runsManyCopies(service: Pick<Service, 'command' | 'env'>): boolean {
  return [service.command, ...(service.env ?? []).map(([, v]) => v)].some((t) => placeholdersIn(t).includes('port'));
}

/** `{checkout}` is a path (Application Support has a space), so it is allowed in env values
 *  only, which `startLine` single-quotes; Rust refuses it in a command at resolve time too. */
export function checkoutInCommand(command: string): boolean {
  return placeholdersIn(command).includes('checkout');
}

/** Why a start of `target` may not go ahead given the copies already up, or null. `live` is
 *  the targets of the copies starting or running, with what each is called. */
export function copyRefusal(
  service: Pick<Service, 'name' | 'command' | 'env'>,
  live: { target: string; label: string }[],
  target: string,
): string | null {
  if (live.some((c) => c.target === target)) return null;
  if (live.length === 0) return null;
  if (!runsManyCopies(service)) {
    return `${service.name} runs one copy at a time (its port is fixed; use {port} in its command to run copies) and it is running on ${live.map((c) => c.label).join(', ')}. Move it instead: Run on ▸ in the sidebar, or restartService with on.`;
  }
  if (live.length >= MAX_COPIES) {
    return `${service.name} already runs ${live.length} copies (${live.map((c) => c.label).join(', ')}), the most it may. Stop one first.`;
  }
  return null;
}

/** Which copy a tool means when it may name one (`on`, already resolved to a target) or not:
 *  the named one; else the only copy running; else the own folder when none is. Several
 *  running and none named is refused with their names — never a guess (docs/hive.md §6.9). */
export function pickCopy(live: { target: string; label: string }[], on: string | null): { target: string } | { ambiguous: string[] } {
  if (on !== null) return { target: on };
  if (live.length === 1) return { target: live[0].target };
  if (live.length === 0) return { target: '' };
  return { ambiguous: live.map((c) => c.label) };
}

/** Every copy of a service worth showing: the own folder's always, then each target named by
 *  a runtime row, a tab, or running intent, first seen first. A service that has only ever
 *  run on its own folder has exactly one: `['']`. */
export function copyTargetsOf(
  serviceId: string,
  runtimeKeys: Iterable<string>,
  tabs: Iterable<{ service_id?: string | null; service_target?: string | null }>,
  runningTargets: string[] | undefined,
): string[] {
  const out = [''];
  const add = (t: string | null | undefined) => { if (t && !out.includes(t)) out.push(t); };
  for (const key of runtimeKeys) {
    const p = parseCopyKey(key);
    if (p.serviceId === serviceId) add(p.target);
  }
  for (const t of tabs) if (t.service_id === serviceId) add(t.service_target);
  for (const t of runningTargets ?? []) add(t);
  return out;
}

/** The write that records whether the human wants a copy up, or null when it already says
 *  so: `was_running` for the own folder, as always; `running_targets` for any other. */
export function intentPatch(
  service: Pick<Service, 'was_running' | 'running_targets'>,
  target: string,
  on: boolean,
): Pick<Service, 'was_running'> | Pick<Service, 'running_targets'> | null {
  if (!target) return (service.was_running ?? false) === on ? null : { was_running: on };
  const have = service.running_targets ?? [];
  if (have.includes(target) === on) return null;
  return { running_targets: on ? [...have, target] : have.filter((t) => t !== target) };
}

/** What comes back without anyone asking: on activation (`auto_start` or left running) or at
 *  launch (left running only), the own folder; and in both, every other copy left running. A
 *  shared service is never started here. */
export function dueCopies(
  service: Pick<Service, 'auto_start' | 'was_running' | 'running_targets' | 'shared_from'>,
  when: 'activation' | 'launch',
): { own: boolean; targets: string[] } {
  if (service.shared_from) return { own: false, targets: [] };
  const own = when === 'activation' ? (service.auto_start ?? true) || !!service.was_running : !!service.was_running;
  return { own, targets: (service.running_targets ?? []).filter(Boolean) };
}

/** Which copies to start, in order, when they come back unasked (`dueCopies`): the ones the
 *  human left running first, then the own folder for `auto_start` only while there is room
 *  for it under `copyRefusal`. A copy whose shell survived a window reload is still up, so it
 *  is not started but does take its room. Without that order a one-copy service moved to a
 *  branch came back on its own folder (auto_start won the race) and the branch was refused. A
 *  service that only ever ran on its own folder gets `['']` exactly when `dueCopies` says. */
export function restorePlan(
  service: Pick<Service, 'auto_start' | 'was_running' | 'running_targets' | 'shared_from' | 'command' | 'env'>,
  when: 'activation' | 'launch',
  survived: (target: string) => boolean,
): string[] {
  if (service.shared_from) return [];
  const room = runsManyCopies(service) ? MAX_COPIES : 1;
  const left = [...new Set([...(service.was_running ? [''] : []), ...dueCopies(service, when).targets])];
  const up = left.filter(survived);
  const out: string[] = [];
  for (const t of left) {
    if (survived(t)) continue;
    if (up.length + out.length >= room) break;
    out.push(t);
  }
  const autoOwn = when === 'activation' && (service.auto_start ?? true);
  if (autoOwn && !out.includes('') && !survived('') && up.length + out.length < room) out.push('');
  return out;
}

/** The intent a start of `target` records. A one-copy service is up in one place, so its
 *  intent names that one and clears the rest: two would race at the next launch and bring it
 *  back on the checkout it was moved away from. Null when nothing changes; otherwise only the
 *  fields that do, so the own folder's write is the one it always was. */
export function startIntent(
  service: Pick<Service, 'was_running' | 'running_targets' | 'command' | 'env'>,
  target: string,
): Partial<Pick<Service, 'was_running' | 'running_targets'>> | null {
  if (runsManyCopies(service)) return intentPatch(service, target, true);
  const patch: Partial<Pick<Service, 'was_running' | 'running_targets'>> = {};
  if ((service.was_running ?? false) !== (target === '')) patch.was_running = target === '';
  const have = service.running_targets ?? [];
  const want = target ? [target] : [];
  if (have.length !== want.length || have.some((t, i) => t !== want[i])) patch.running_targets = want;
  return Object.keys(patch).length ? patch : null;
}

/** What Run on ▸ / `restartService { on }` does to put a service on `to` (docs/hive.md §6.9).
 *  A copy "held" is one not stopped: up, starting, or crashed (its restart may still be armed,
 *  and its intent is still set), so a crashed copy is MOVED like a live one rather than left
 *  behind to restart beside the new one. `from` is the row the menu was opened on, if any. */
export function moveVerb(
  service: Pick<Service, 'command' | 'env'>,
  copies: { target: string; live: boolean; status: ServiceStatus }[],
  to: string,
  from: string | null,
): { verb: 'restart' } | { verb: 'retarget'; from: string } | { verb: 'start' } {
  const held = copies.filter((c) => c.live || c.status !== 'stopped');
  if (from !== null && from !== to && held.some((c) => c.target === from)) return { verb: 'retarget', from };
  if (held.some((c) => c.target === to)) return { verb: 'restart' };
  if (held.length > 0 && !runsManyCopies(service)) return { verb: 'retarget', from: (held.find((c) => c.live) ?? held[0]).target };
  return { verb: 'start' };
}

/** The row the store publishes to Rust for a copy (docs/stack.md §6.2). The own folder's row
 *  is exactly what it was before copies existed; another copy's also says where it runs. */
export function publishRow(
  key: string,
  r: { status: ServiceStatus; note: string | null; since: number | null; copy?: { cwd: string; label: string } | null },
): StackRuntimeRow {
  const { serviceId, target } = parseCopyKey(key);
  const row: StackRuntimeRow = { service_id: serviceId, status: r.status, note: r.note, since_ms: r.since };
  if (target) Object.assign(row, { target, cwd: r.copy?.cwd ?? '', label: r.copy?.label ?? '' });
  return row;
}

/** A service another workspace is running, as the Hive sheet's Stack step weighs it. */
export interface RunningElsewhere {
  name: string;
  workspace: string;
  command: string;
  cwd: string;
  port: number | null;
}

/** What importing a suggested definition into a Hive does (docs/hive.md §3 step 3): with
 *  `{port}` it runs a copy per branch; with a fixed port it runs one copy at a time — unless
 *  another workspace already runs the same command in the same folder on a known port, which
 *  the Hive would fight for, so it is refused and that service named. Rust (`import_stack`)
 *  re-checks the refusal against every window. */
export function importVerdict(
  row: { command: string; cwd: string },
  running: RunningElsewhere[],
): { kind: 'copies' | 'fixed' | 'refused'; detail: string } {
  if (runsManyCopies({ command: row.command, env: [] })) return { kind: 'copies', detail: 'a copy per branch, each on its own port' };
  const clash = running.find((r) => r.port != null && r.command.trim() === row.command.trim() && r.cwd === row.cwd);
  if (clash) {
    return { kind: 'refused', detail: `${clash.name} in ${clash.workspace} runs this on :${clash.port}. Add {port} to the command, or use the running one.` };
  }
  return { kind: 'fixed', detail: 'fixed port: runs on one branch at a time' };
}

/** A suggestion for the Hive sheet, as `suggest_stack` makes it. */
export interface StackOffer {
  name: string;
  command: string;
  cwd: string;
  source: string;
}

/** The running service and the suggestion are the same service: same command in the same
 *  folder, or the same name (which could not both be imported anyway). */
export function sameService(a: { name: string; command: string; cwd: string }, b: { name: string; command: string; cwd: string }): boolean {
  return (a.command.trim() === b.command.trim() && a.cwd === b.cwd) || normalizeTitle(a.name) === normalizeTitle(b.name);
}

/** Whether a service another workspace runs should be used from there by default (docs/hive.md
 *  §3 step 3, §6.8). Only infrastructure: one that is the repo's own code — it matches a
 *  suggestion from package.json, a Procfile or a recipe file, anything but compose — has to run
 *  per branch, and a reference could never be moved to one. */
export function sharesByDefault(running: { name: string; command: string; cwd: string }, offers: StackOffer[]): boolean {
  return !offers.some((o) => o.source !== 'compose' && sameService(running, o));
}

// ── Who may edit a definition (Hive spike 15) ──────────────────────────────────────

/** `updateService` fields that rewrite what runs. An agent sets them only on a service an
 *  agent defined; on the human's it may only report `port`, `url`, `ready` and `note`. */
export const DEFINITION_FIELDS = ['name', 'command', 'cwd', 'env', 'auto_start', 'restart', 'ready_pattern'] as const;

/** The human wrote it and no agent ever has: what a Hive worker may run, and what agents
 *  may not rewrite. */
export function isHumanDefined(service: Pick<Service, 'origin' | 'agent_edited'>): boolean {
  return service.origin !== 'agent' && !service.agent_edited;
}

/** The definition fields an agent's `updateService` call is refused, in the tool's order
 *  (empty when it may make them). */
export function refusedAgentEdits(service: Pick<Service, 'origin' | 'agent_edited'>, args: Record<string, unknown>): string[] {
  if (!isHumanDefined(service)) return [];
  return DEFINITION_FIELDS.filter((f) => args[f] !== undefined);
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
