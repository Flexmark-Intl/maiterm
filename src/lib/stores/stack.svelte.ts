/** Per-window stack store (docs/stack.md).
 *
 *  A workspace declares the services its project runs; each running service IS a terminal
 *  tab whose shell stays up while the command runs in front of it (§4). This store owns
 *  the definitions' edits (persisted through `setWorkspaceStack`, a whole-list replace like
 *  tasks), and ALL of the runtime state — status, uptime, the foreground pid — which is
 *  never persisted: a parked workspace would otherwise wake up "crashed".
 *
 *  Every write into a terminal goes through the guard in §4, and the guard is the shell's
 *  OWN OSC 133 sequence, not a probe of the tty: a start is typed only after the shell's
 *  prompt (A) — its own, per PTY, never a dead shell's — and only when no command has begun
 *  (B/C) since; the command is ours once the first B/C after our write arrives; a D counts
 *  as our exit only after that; a stop signals only the pid recorded then. The tty
 *  foreground (`get_pty_foreground_job`) captures that pid and is the fallback for shells
 *  with no integration — it cannot tell "idle at a prompt" from "still in .zshrc", which
 *  is why it is not the guard. A wrong "no" costs a retry; a wrong "yes" types into, or
 *  SIGTERMs, whatever the human left running there. Four review rounds shaped this; the
 *  as-built table in the doc records what each one caught.
 *
 *  A service can run as several COPIES, one per checkout (docs/hive.md §6.9, docs/stack.md
 *  §14): every runtime map here — status, the in-flight starts and stops, restart timers,
 *  the console, the bound tab — is keyed by the copy key (`stack/model.ts` `copyKey`). The
 *  own folder's key is the bare service id, so a service that never runs anywhere else keys,
 *  starts and stops exactly as it did before copies existed.
 *
 *  A Hive's run queue (docs/hive.md §6.8) runs its JOBS through the same tabs and guard: Rust
 *  decides when, builds the line, and reads the exit off the PTY itself; this store mints the
 *  hidden tab, types the line (`startJob`) and ^C's it (`stopJob`). A job's row lives only in
 *  `jobRows`, never in a workspace's stack mirror, and a job never restarts or records intent.
 *  A copy a job depends on is LEASED: `stop` and `retarget` refuse it with a `LeaseHeldError`,
 *  and `runWhenFree` / `stopAnyway` are the two ways past.
 */

import { untrack } from 'svelte';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { error as logError, info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { ResolvedCopy, StackSharedRow } from '$lib/tauri/commands';
import type { Service, ServiceOrigin, ServiceRef, ServiceRestart, StackHeldAction, StackLeaseState, Tab, Workspace } from '$lib/tauri/types';
import { workspacesStore } from './workspaces.svelte';
import { terminalsStore } from './terminals.svelte';
import { activityStore } from './activity.svelte';
import { preferencesStore } from './preferences.svelte';
import { normalizeTitle } from '$lib/tasks/model';
import {
  checkoutInCommand, copyKey, copyRefusal, copyTabName, copyTargetsOf, detectEndpoint, exitIsCrash, intentPatch, moveVerb,
  parseCopyKey, publishRow, restartAllowed, restartDelay, restorePlan, rollupStatus, runsManyCopies, startIntent, startLine, targetLabel,
  RESTART_CEILING, RESTART_WINDOW_MS, type Rollup, type ServiceStatus,
} from '$lib/stack/model';
import { stripAnsi } from '$lib/utils/ansi';

export type { ServiceStatus } from '$lib/stack/model';

export interface ServiceRuntime {
  status: ServiceStatus;
  /** ms epoch of the last transition into starting/running/ready; null when stopped. */
  since: number | null;
  lastExitCode: number | null;
  /** Foreground job leader recorded after start — the stop guard (§4). */
  pid: number | null;
  /** The PTY the command was typed into. A bound tab whose live PTY is a different one
   *  (reload, respawn) is a fresh shell with nothing running in it. */
  ptyId: string | null;
  /** ms epoch of the write; the B/C that begins OUR command must come after it. */
  writeAt: number | null;
  /** ms epoch of the B/C observed after `writeAt` — the command is running. Until this is
   *  set, no D on the tab is ours (the fresh shell's first-prompt D;0, a human's `ls`). */
  beganAt: number | null;
  /** The shell showed no OSC 133 at all; exits are inferred from the tty foreground. */
  noIntegration: boolean;
  /** A stop is in flight: the next exit is `stopped`, never `crashed`. */
  stopping: boolean;
  /** ms epochs of automatic restarts, for the ceiling. */
  restarts: number[];
  /** One line of why, for the sidebar and `listStack`. */
  note: string | null;
  /** Where the CURRENT run's endpoint came from: `observed` is maiTerm reading the
   *  service's own output, `reported` is an agent calling `updateService`. Null until
   *  this run announces one, which is what makes a persisted port read as stale (§9).
   *  Runtime, not persisted — provenance belongs to a run, not to a definition. */
  endpointFrom: 'observed' | 'reported' | null;
  /** What Rust resolved this copy to at its last start (`stack_resolve_copy`): the folder,
   *  the ports it booked, the branch and commit, what to call it. Null until a start. */
  copy: ResolvedCopy | null;
  /** A copy's OWN endpoint, for a copy on another checkout. The definition's `port`/`url` are
   *  the own folder's, persisted; a copy's address belongs to its run and is never written
   *  there. Always null for the own folder's copy. */
  endpoint: { port: number | null; url: string | null } | null;
}

/** One copy of a service, as the sidebar, the console and `listStack` show it. */
export interface CopyView {
  key: string;
  serviceId: string;
  /** `''` for the own folder (docs/hive.md §6.9). */
  target: string;
  /** `main` for the own folder, else the worker, branch or folder. */
  label: string;
  runtime: ServiceRuntime;
  tab: Tab | null;
  /** Starting or up, or a start in flight. */
  live: boolean;
}

export interface ServiceInput {
  name: string;
  command: string;
  cwd: string;
  env?: [string, string][];
  auto_start?: boolean;
  restart?: ServiceRestart;
  ready_pattern?: string | null;
  origin?: ServiceOrigin;
  /** Another workspace's service, used here (docs/hive.md §6.8): never started from here. */
  shared_from?: ServiceRef | null;
}

/** A stop or retarget of a copy a running job depends on (docs/hive.md §6.8). Never applied
 *  mid-run: the caller offers Run when free (`runWhenFree`) or Stop anyway (`stopAnyway`). */
export class LeaseHeldError extends Error {
  constructor(public readonly copyKey: string, public readonly lease: StackLeaseState, label: string) {
    const job = lease.job!;
    super(`${label} is in use by a queued run (${job.entry} for ${job.worker_slug}, ${job.id}); it can run when that's free, or be stopped anyway`);
    this.name = 'LeaseHeldError';
  }
}

/** The human's choice a `LeaseHeldError` asks for (`LeaseHeldChoice.svelte`): the copy it hit,
 *  and the stop, restart or move to run when free or anyway. */
export interface LeaseAsk {
  workspaceId: string;
  serviceId: string;
  target: string;
  action: 'stop' | 'restart' | 'retarget';
  /** Where a move goes. */
  to: string | null;
  message: string;
}

/** The choice to offer for an error a stack verb threw, or null when it isn't a lease. */
export function leaseAskOf(e: unknown, workspaceId: string, action: LeaseAsk['action'], to: string | null = null): LeaseAsk | null {
  if (!(e instanceof LeaseHeldError)) return null;
  const { serviceId, target } = parseCopyKey(e.copyKey);
  return { workspaceId, serviceId, target, action, to, message: e.message };
}

/** What a job start answers its rpc (`stack.job.start`): `confirmed` once the shell's own B for
 *  the typed line was seen. Rust records the exit itself. */
export interface JobStartAnswer {
  accepted?: boolean;
  confirmed?: boolean;
  tab_id?: string;
  pty_id?: string;
  error?: string;
}

/** Rust's word for a shell with no OSC 133 (`queue::NEEDS_INTEGRATION`): never a pass. */
const NEEDS_INTEGRATION = 'needs shell integration';

const IDLE: ServiceRuntime = { status: 'stopped', since: null, lastExitCode: null, pid: null, ptyId: null, writeAt: null, beganAt: null, noIntegration: false, stopping: false, restarts: [], note: null, endpointFrom: null, copy: null, endpoint: null };

/** How often a `shared_from` service's status is read from the Rust mirror (docs/hive.md §6.8). */
const SHARED_POLL_MS = 5000;

function isLive(status: ServiceStatus): boolean {
  return status === 'starting' || status === 'running' || status === 'ready';
}

/** Everything the store has to remember about a SHELL, from the raw OSC 133 feed. Keyed
 *  by PTY id, never tab id: a tab id outlives its shell (suspend/resume respawns under the
 *  same id; reload copies the binding to a new id), and a respawned shell must earn its
 *  own first prompt — the dead shell's A said nothing about the rc this one is still in. */
interface ShellFacts {
  /** ms epoch of the last A. A fresh shell has none until its rc finishes. */
  lastPromptAt: number | null;
  /** ms epoch of the last B/C. */
  lastBeginAt: number | null;
  /** ms epoch of the last D. */
  lastExitAt: number | null;
}

/** How long a freshly mounted tab gets to show its first prompt (A) before we conclude the
 *  shell has no integration and fall back to the tty-foreground heuristic. Real rcs
 *  (oh-my-zsh + nvm + conda) take 2–3s; 12s is generous without being forever. */
const FIRST_PROMPT_WAIT_MS = 12000;
/** After the write, how long until the shell's B/C must have arrived. Generous: a prompt
 *  hook (direnv evaluating a fresh .envrc) can sit between Enter and preexec, and giving up
 *  early is worse than waiting — the line is queued in the tty and WILL run, and a start
 *  that has already disowned it leaves a running service nothing can stop. */
const BEGIN_WAIT_MS = 30000;
const MOUNT_WAIT_MS = 15000;
/** How long the shell must sit at its prompt, with nothing begun, before a start types. */
const PROMPT_SETTLE_MS = 750;
/** The two ways a bound tab loses the shell our command ran in; reconcileSuspended
 *  corrects the first into the second when the workspace turns out to be suspended. */
const RELOADED_NOTE = 'its tab was reloaded — start it again';
const SUSPENDED_NOTE = 'its tab was suspended';

function sleep(ms: number) {
  return new Promise<void>((r) => setTimeout(r, ms));
}

function createStackStore() {
  /** By copy key: the service id for its own folder (docs/hive.md §6.9). */
  let runtime = $state<Map<string, ServiceRuntime>>(new Map());
  /** A `shared_from` service's source, as the Rust mirror last said, by the LOCAL service id.
   *  Never put in `runtime`: it is not this window's to publish, restart or stop. */
  let shared = $state<Map<string, StackSharedRow | null>>(new Map());
  /** Which copy the console drawer is showing, per workspace. Session-only, like every
   *  other stack runtime fact (§3): a drawer left open is not worth restoring, and it must
   *  never be the reason a service tab survives a restart. */
  let consoleService = $state<Map<string, string>>(new Map());
  /** The same thing, lagging a closed drawer by its slide-out (ServiceConsole.svelte).
   *
   *  It is NOT "is the console open" — Escape, the sidebar's Hide/Show label and
   *  `toggleConsole` all key off `consoleService`, which empties the moment you close.
   *  This is "what is still on screen", and it has to live in the store rather than in
   *  the drawer because two components have to agree on it: the drawer renders from it,
   *  and `+page` derives the service terminal's `visible` prop from it. Keeping it local
   *  to the drawer is the bug it was written to fix — `+page` hid the terminal at t=0 and
   *  the drawer slid out as an empty box. */
  let consoleVisible = $state<Map<string, string>>(new Map());
  const consoleCloseTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** Comfortably longer than the drawer's 160ms transition: the transition does not start
   *  until the class change reaches a style recalc, so matching the two exactly unmounts
   *  the drawer a frame short and blinks its last sliver away. Overshooting only delays
   *  the unmount of something already off screen. */
  const CONSOLE_CLOSE_MS = 220;
  /** Workspaces whose auto_start already fired for this activation. Cleared on suspend. */
  const autoStarted = new Set<string>();
  /** The window's state has finished loading (`onLoaded`). Auto-start waits for it: the
   *  active workspace id is set BEFORE `load()` learns which PTYs survived a reload, and a
   *  start decided in that gap types into — or is refused by — a still-running service. */
  let loaded = false;
  const restartTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let sharedTimer: ReturnType<typeof setInterval> | null = null;
  /** Starts in flight, by copy key. The status alone cannot guard re-entry: `start` does
   *  four IPC round trips before it can set `starting`, and a double-click or two parallel
   *  agent calls land inside that window and mint a second tab. A second caller gets the
   *  SAME promise rather than a silent no-op, so an auto-restart timer or a human's second
   *  click never evaporates; `stop` awaits it (after asking it to abort) instead of
   *  returning without doing anything. */
  const startsInFlight = new Map<string, Promise<ServiceStatus>>();
  const stopsInFlight = new Map<string, Promise<ServiceStatus>>();
  /** A stop arrived while the start was still before its write: abort instead of typing. */
  const abortStart = new Set<string>();
  const shellFacts = new Map<string, ShellFacts>();
  /** Run-queue jobs this window started, by job id (which is also the copy key): the row Rust
   *  inserted, kept here and never in a workspace's stack mirror, whose whole-list writes
   *  would carry it back. Dropped when its tab is cleaned up. */
  const jobRows = new Map<string, { workspaceId: string; row: Service }>();
  /** Every job id seen this session: their runtime rows are never published to the mirror. */
  const jobKeys = new Set<string>();
  /** Tail of a service tab's output while we are still looking for its address, so a URL
   *  split across two PTY chunks is still seen whole. Dropped the moment a run announces
   *  one — this is not a log buffer and must never grow into one. */
  const scanTails = new Map<string, string>();
  const SCAN_TAIL_CHARS = 512;
  /** How long into a run we keep looking for an address. A server announces itself while
   *  it is starting — generous enough for a cold webpack build, short of "forever" for a
   *  queue worker that prints all day and never serves anything. */
  const SCAN_WINDOW_MS = 5 * 60 * 1000;
  /** Compiled `ready_pattern`s by service id, rebuilt only when the pattern text changes.
   *  A regex that does not compile is complained about once and then treated as absent: a
   *  typo in that field must not silence the built-in detection too. */
  const readyPatterns = new Map<string, { src: string; re: RegExp | null }>();
  let unsubscribe: (() => void)[] = [];

  function readyPattern(service: Service): RegExp | null {
    const src = service.ready_pattern?.trim();
    if (!src) return null;
    let entry = readyPatterns.get(service.id);
    if (!entry || entry.src !== src) {
      let re: RegExp | null = null;
      try {
        re = new RegExp(src);
      } catch (e) {
        logWarn(`stack: ${service.name} has an unusable ready_pattern (${e}) — ignoring it`);
      }
      entry = { src, re };
      readyPatterns.set(service.id, entry);
    }
    return entry.re;
  }

  function facts(ptyId: string): ShellFacts {
    let f = shellFacts.get(ptyId);
    if (!f) { f = { lastPromptAt: null, lastBeginAt: null, lastExitAt: null }; shellFacts.set(ptyId, f); }
    return f;
  }

  /** Poll `facts` until `pred` holds or the deadline passes. Event-driven would be nicer;
   *  150ms polling is invisible next to a shell rc and keeps this a plain loop. */
  async function waitForFact(ptyId: string, pred: (f: ShellFacts) => boolean, timeoutMs: number): Promise<boolean> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (pred(facts(ptyId))) return true;
      await sleep(150);
    }
    return pred(facts(ptyId));
  }

  function rt(key: string): ServiceRuntime {
    return runtime.get(key) ?? IDLE;
  }

  /** Free the ports Rust booked for a copy's `{port}`/`{port2}` (docs/hive.md §6.9). The book
   *  still offers them to the copy first next time; a service with no `{port}` booked none and
   *  costs no call. */
  function releasePorts(key: string, r: ServiceRuntime) {
    if (r.copy?.port == null && r.copy?.port2 == null) return;
    commands.stackReleaseCopy(key).catch((e) => logWarn(`stack: release ports of ${key}: ${e}`));
  }

  /** Told of every status CHANGE, after it is applied — follow-ups wait on these (docs/follow-ups.md
   *  §5). Edges, never levels: status is never persisted and reads `stopped` for a service nothing
   *  has reported, so only a change this store makes is evidence that something happened. The
   *  key is the copy key, which for the own folder IS the service id: a follow-up waits on that
   *  copy, as it always has. */
  const transitionListeners = new Set<(key: string, from: ServiceStatus, to: ServiceRuntime) => void>();

  function setRt(key: string, patch: Partial<ServiceRuntime>) {
    const before = rt(key);
    const after = { ...before, ...patch };
    const next = new Map(runtime);
    next.set(key, after);
    runtime = next;
    if (after.status !== before.status) {
      if (!isLive(after.status) && isLive(before.status)) releasePorts(key, after);
      for (const fn of transitionListeners) {
        try { fn(key, before.status, after); } catch (e) { logError(`stack: transition listener: ${e}`); }
      }
    }
  }

  function workspaceOf(workspaceId: string): Workspace | undefined {
    return workspacesStore.workspaces.find((w) => w.id === workspaceId);
  }

  function serviceOf(workspaceId: string, serviceId: string): Service | undefined {
    return workspaceOf(workspaceId)?.stack?.find((s) => s.id === serviceId);
  }

  function boundIn(ws: Workspace, serviceId: string, target: string): { pane: { id: string }; tab: Tab } | undefined {
    for (const pane of ws.panes) {
      const tab = pane.tabs.find((t) => t.service_id === serviceId && (t.service_target ?? '') === target);
      if (tab) return { pane, tab };
    }
    return undefined;
  }

  /** The tab running a copy — derived from `Tab.service_id` + `Tab.service_target`, never
   *  stored (§3). No target is the own folder's copy. */
  function boundTab(workspaceId: string, serviceId: string, target = ''): { pane: { id: string }; tab: Tab } | undefined {
    const ws = workspaceOf(workspaceId);
    return ws ? boundIn(ws, serviceId, target) : undefined;
  }

  /** A job this window lost track of: a window reload keeps the job's PTY running and loads
   *  the row Rust inserted for it, but empties `jobRows`. Taken back as a job, never a service. */
  function adoptJob(ws: Workspace, jobId: string): { workspaceId: string; row: Service } | undefined {
    const row = ws.stack?.find((s) => s.id === jobId && s.kind === 'job');
    if (!row) return undefined;
    const job = { workspaceId: ws.id, row };
    jobRows.set(jobId, job);
    jobKeys.add(jobId);
    return job;
  }

  /** Reverse lookup for the exit hooks: which copy of which service does this tab run? */
  function serviceForTab(tabId: string): { workspaceId: string; service: Service; target: string; key: string; job?: true } | undefined {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find((t) => t.id === tabId);
        if (!tab) continue;
        if (!tab.service_id) return undefined;
        const job = jobRows.get(tab.service_id) ?? adoptJob(ws, tab.service_id);
        if (job && job.workspaceId === ws.id) return { workspaceId: ws.id, service: job.row, target: '', key: tab.service_id, job: true };
        const service = ws.stack?.find((s) => s.id === tab.service_id);
        const target = tab.service_target ?? '';
        return service ? { workspaceId: ws.id, service, target, key: copyKey(service.id, target) } : undefined;
      }
    }
    return undefined;
  }

  function labelOf(key: string): string {
    return rt(key).copy?.label || targetLabel(parseCopyKey(key).target);
  }

  /** Every copy of a service worth showing: the own folder's always, then each other one that
   *  has a runtime row, a tab, or running intent — in that order, first seen first. */
  function copiesOf(workspaceId: string, serviceId: string): CopyView[] {
    const ws = workspaceOf(workspaceId);
    const service = ws?.stack?.find((s) => s.id === serviceId);
    if (!ws || !service) return [];
    const targets = copyTargetsOf(serviceId, runtime.keys(), ws.panes.flatMap((p) => p.tabs), service.running_targets);
    return targets.map((target) => {
      const key = copyKey(serviceId, target);
      const r = rt(key);
      return { key, serviceId, target, label: labelOf(key), runtime: r, tab: boundIn(ws, serviceId, target)?.tab ?? null, live: isLive(r.status) || startsInFlight.has(key) };
    });
  }

  function liveCopies(workspaceId: string, serviceId: string): CopyView[] {
    return copiesOf(workspaceId, serviceId).filter((c) => c.live);
  }

  /** Where another workspace's service lives, named for a refusal. */
  function sharedRefusal(service: Service): string {
    const from = shared.get(service.id)?.workspace_name
      ?? workspacesStore.workspaces.find((w) => w.id === service.shared_from?.workspace_id)?.name
      ?? 'another workspace';
    return `${service.name} is shared from ${from}; it's started and stopped there`;
  }

  async function persist(workspaceId: string, stack: Service[]) {
    try {
      await workspacesStore.setWorkspaceStack(workspaceId, stack);
    } catch (e) {
      logError(`stack: persist failed for workspace ${workspaceId}: ${e}`);
    }
  }

  /** A window RELOAD, not a launch: the service's shell survived (its PTY is still live in
   *  Rust) and is most likely still running it. A start would wait out the guard and file
   *  it `stopped` while it serves — and with no live status, neither Stop nor a Ctrl-C
   *  could clear its intent after (docs/stack.md §12.8). */
  function survivedReload(workspaceId: string, serviceId: string, target = ''): boolean {
    const tab = boundTab(workspaceId, serviceId, target)?.tab;
    return !!tab && terminalsStore.wasLiveAtLoad(tab.pty_id);
  }

  /** Record whether the human wants this copy up: `Service.was_running` for the own folder,
   *  its target in `Service.running_targets` for any other (docs/hive.md §6.9). Only the
   *  verbs and a clean exit write it: a crash, a suspend or the app quitting leave it set,
   *  which is what lets the next launch put the stack back the way it was left — each copy on
   *  its own checkout. Not a definition edit, so `updated_at` is left alone. */
  function markIntent(workspaceId: string, serviceId: string, target: string, on: boolean) {
    const stack = workspaceOf(workspaceId)?.stack ?? [];
    const current = stack.find((s) => s.id === serviceId);
    const patch = current ? intentPatch(current, target, on) : null;
    if (!patch) return;
    void persist(workspaceId, stack.map((s) => (s.id === serviceId ? { ...s, ...patch } : s)));
  }

  /** A start of this copy is going ahead: record it (`startIntent`). A one-copy service is
   *  now up here and nowhere else, so a crash left on another copy is answered too — its
   *  armed restart would only be refused, and its intent would bring it back at launch. */
  function claimIntent(workspaceId: string, serviceId: string, target: string) {
    const current = serviceOf(workspaceId, serviceId);
    if (!current) return;
    if (!runsManyCopies(current)) {
      for (const c of copiesOf(workspaceId, serviceId)) if (c.target !== target && c.runtime.status === 'crashed') settleCrash(c.key);
    }
    const patch = startIntent(current, target);
    if (!patch) return;
    const stack = workspaceOf(workspaceId)?.stack ?? [];
    void persist(workspaceId, stack.map((s) => (s.id === serviceId ? { ...s, ...patch } : s)));
  }

  /** A crashed copy that won't be restarted: its backoff timer is disarmed (it re-checks
   *  `crashed` when it fires, so leaving the status alone would let it restart) and the red
   *  dot is answered. The exit code stays for listStack. */
  function settleCrash(key: string) {
    const r = rt(key);
    const timer = restartTimers.get(key);
    if (timer) { clearTimeout(timer); restartTimers.delete(key); }
    // scheduleRestart books the restart when it ARMS the timer; a cancelled one must not
    // count against the ceiling.
    setRt(key, { status: 'stopped', note: null, restarts: timer ? r.restarts.slice(0, -1) : r.restarts });
  }

  // ── Runtime transitions ────────────────────────────────────────────────────────

  async function probeForeground(ptyId: string) {
    try {
      return await commands.getPtyForegroundJob(ptyId, true);
    } catch {
      return null;
    }
  }

  /** Poll until no external job holds the tty (`shell_at_prompt === true`). NOTE this is
   *  NOT "idle at a prompt" — builtins, rc files and command substitutions never change the
   *  tty's foreground group. It is only ever used to wait for a job WE signalled to leave
   *  the foreground, or as the fallback for a shell with no OSC 133 at all. */
  async function waitForPrompt(ptyId: string, timeoutMs: number): Promise<boolean> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const fg = await probeForeground(ptyId);
      if (fg?.shell_at_prompt === true) return true;
      await sleep(250);
    }
    return false;
  }

  function onShellPrompt(_tabId: string, ptyId: string) {
    facts(ptyId).lastPromptAt = Date.now();
  }

  function onCommandBegin(tabId: string, ptyId: string) {
    const now = Date.now();
    facts(ptyId).lastBeginAt = now;
    const hit = serviceForTab(tabId);
    if (!hit) return;
    const r = rt(hit.key);
    // The B/C for OUR line: the first one after the write, on the PTY we wrote to.
    // Anything earlier is a command the shell was already running (or the rc's own
    // hooks) and says nothing about us.
    if (r.writeAt !== null && r.beganAt === null && r.ptyId === ptyId && now >= r.writeAt && r.status === 'starting') {
      setRt(hit.key, { beganAt: now, status: 'running' });
    }
  }

  /** `exitCode === null` is the no-integration fallback: the tty came back with nothing
   *  to say why. With integration, a D counts only once the command's own B/C was seen. */
  function onExit(tabId: string, ptyId: string, exitCode: number | null) {
    facts(ptyId).lastExitAt = Date.now();
    const hit = serviceForTab(tabId);
    if (!hit) return;
    const { workspaceId, service, target, key } = hit;
    const r = rt(key);
    if (r.status === 'stopped' || r.status === 'crashed') return;
    if (r.ptyId !== null && r.ptyId !== ptyId) return; // a different shell's exit
    if (exitCode !== null && !r.noIntegration && r.beganAt === null) {
      // A D before our command began: the fresh shell's unconditional first-prompt D;0,
      // or a human's command finishing on the bound tab while we waited. Not ours.
      return;
    }
    if (hit.job) {
      // Rust records a job's exit from the shell's own marks (hive::queue). The job can print a
      // D of its own, so here it reads as over only once the shell holds the terminal again;
      // a D while the job is still in front changes nothing, and a later stop still ^C's it.
      // Never crashed, never restarted, no intent. `beganAt` stays, so a start still waiting on
      // its B can tell a job that already finished from one that never ran.
      void probeForeground(ptyId).then((fg) => {
        if (fg?.shell_at_prompt !== true || rt(key).ptyId !== ptyId || !isLive(rt(key).status)) return;
        setRt(key, { status: 'stopped', since: null, pid: null, stopping: false, lastExitCode: exitCode, note: exitCode === null ? null : `exit ${exitCode}` });
      });
      return;
    }
    // A deliberate stop is a stop whatever the code — `make dev` returns 2 when its child
    // is interrupted.
    const crashed = !r.stopping && (exitCode === null || exitIsCrash(exitCode));
    setRt(key, {
      status: crashed ? 'crashed' : 'stopped',
      since: null,
      pid: null,
      ptyId: null,
      writeAt: null,
      beganAt: null,
      stopping: false,
      lastExitCode: exitCode,
      note: crashed ? (exitCode === null ? 'exited (no exit code — shell has no integration)' : `exit ${exitCode}`) : null,
    });
    if (crashed) {
      logWarn(`stack: ${service.name}${target ? ` on ${labelOf(key)}` : ''} crashed with exit ${exitCode}`);
      scheduleRestart(workspaceId, service, target);
    } else {
      // A Ctrl-C in the console, or the program finishing on its own: off is the answer.
      markIntent(workspaceId, service.id, target, false);
    }
  }

  function scheduleRestart(workspaceId: string, service: Service, target: string) {
    if ((service.restart ?? 'on_crash') !== 'on_crash') return;
    const key = copyKey(service.id, target);
    const r = rt(key);
    const now = Date.now();
    if (!restartAllowed(r.restarts, now)) {
      setRt(key, { note: `crashed ${RESTART_CEILING}× in 10 min — not restarting` });
      return;
    }
    const recent = r.restarts.filter((t) => now - t < RESTART_WINDOW_MS);
    const delay = restartDelay(recent.length);
    setRt(key, { restarts: [...recent, now], note: `restarting in ${Math.round(delay / 1000)}s` });
    const existing = restartTimers.get(key);
    if (existing) clearTimeout(existing);
    restartTimers.set(key, setTimeout(() => {
      restartTimers.delete(key);
      if (rt(key).status !== 'crashed') return;
      start(workspaceId, service.id, target).catch((e) => logError(`stack: auto-restart of ${service.name} failed: ${e}`));
    }, delay));
  }

  /** A bound tab that disappears (its shell died and `pty-close` deleted it, or the human
   *  closed it) takes its running service down with it — `crashed` unless a stop was in
   *  flight. No restart: there is no tab to restart in; the next start mints one. A bound
   *  tab whose live PTY is not the one the command was typed into (a reload minted a fresh
   *  shell) is `stopped`: nothing is running there, and a green dot over it would lie. */
  function reconcileBindings() {
    for (const [key, r] of runtime) {
      const { serviceId, target } = parseCopyKey(key);
      // The binding only counts inside the workspace that owns the definition — a tab
      // moved elsewhere has its binding cleared by Rust, and searching every workspace
      // would keep a moved-away service "running" here with no tab to show for it.
      // A job's row is in `jobRows`, never in a stack mirror.
      const job = jobRows.get(serviceId);
      const ws = job ? workspaceOf(job.workspaceId) : workspacesStore.workspaces.find((w) => w.stack?.some((s) => s.id === serviceId));
      const tab = ws ? boundIn(ws, serviceId, target)?.tab : undefined;
      if (r.status === 'stopped') {
        // suspendTab unregisters the instance BEFORE it stamps `suspended_at`, so the pass
        // that saw the instance vanish filed a reload; the stamp arrives a tick later and
        // the effect tracks it — correct the note here, as reconcileSuspended does for a
        // whole workspace.
        if (r.note === RELOADED_NOTE && tab?.suspended_at) setRt(key, { note: SUSPENDED_NOTE });
        continue;
      }
      if (r.status === 'crashed') continue;
      // A start in flight owns its runtime until it returns: it is between "chose a tab"
      // and "typed into it", and any reading of its half-written fields here is wrong.
      if (startsInFlight.has(key)) continue;
      if (!tab) {
        // A job is never `crashed`: Rust records how it ended.
        const crashed = !r.stopping && !job;
        setRt(key, { status: crashed ? 'crashed' : 'stopped', since: null, pid: null, ptyId: null, stopping: false, note: crashed ? 'its tab closed' : null });
        if (crashed) logWarn(`stack: service ${key} lost its tab`);
        continue;
      }
      // `ptyId` is recorded only once the command has been typed into a live PTY, so a
      // bound tab that now has no instance (suspendTab killed it) or a different one (a
      // reload minted a fresh shell) has nothing of ours running in it.
      const live = terminalsStore.get(tab.id);
      if (r.ptyId && (!live || live.ptyId !== r.ptyId)) {
        // The reload case is reconciled BEFORE the fresh tab's pane has mounted, so "is
        // there a live instance" reads as suspended for both; the tab's own record tells
        // them apart (`suspended_at` is set by a suspend and by nothing else).
        const suspended = !!tab.suspended_at || !!ws?.suspended;
        setRt(key, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: suspended ? SUSPENDED_NOTE : RELOADED_NOTE });
      }
    }
  }

  /** Suspending a workspace kills its PTYs: every copy of every service in it is `stopped`,
   *  and the workspace may auto-start again on its next activation. */
  function reconcileSuspended() {
    for (const ws of workspacesStore.workspaces) {
      if (!ws.suspended) continue;
      autoStarted.delete(ws.id);
      const ids = new Set((ws.stack ?? []).map((s) => s.id));
      // The own folder's copy is visited even with no row, as it always was; it reads IDLE.
      const keys = new Set([...ids, ...[...runtime.keys()].filter((k) => ids.has(parseCopyKey(k).serviceId))]);
      for (const key of keys) {
        const r = rt(key);
        // A suspend kills the PTYs first and flags the workspace second, so
        // reconcileBindings has usually filed the service as "reloaded" by the time this
        // runs; the flag is the truth, and "start it again" is wrong advice here.
        if (r.status !== 'stopped') setRt(key, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: SUSPENDED_NOTE });
        else if (r.note === RELOADED_NOTE) setRt(key, { note: SUSPENDED_NOTE });
      }
    }
  }

  // ── Verbs ──────────────────────────────────────────────────────────────────────

  /** `replacing`: the copy a `retarget` will stop once this one is up, which the copy limit
   *  counts as gone. */
  async function start(workspaceId: string, serviceId: string, target = '', replacing: string | null = null): Promise<ServiceStatus> {
    const service = serviceOf(workspaceId, serviceId);
    if (!service) throw new Error('Service not found');
    if (service.shared_from) throw new Error(sharedRefusal(service));
    const ws = workspaceOf(workspaceId)!;
    if (ws.suspended) throw new Error('Workspace is suspended — resume it first');
    const key = copyKey(serviceId, target);
    // Before any intent is recorded: a copy that may not run must not come back at launch. A
    // service only ever started on its own folder has no other copy, so this is always null.
    const refusal = copyRefusal(service, liveCopies(workspaceId, serviceId).filter((c) => c.target !== replacing), target);
    if (refusal) throw new Error(refusal);
    // The own folder records intent here, as it always has. Another copy records it only once
    // Rust has resolved it and it has a tab (`startInner`): a mistyped branch, refused by the
    // resolve, would otherwise be retried at every launch.
    if (!target) claimIntent(workspaceId, serviceId, target);
    const pending = startsInFlight.get(key);
    if (pending) return pending;
    const stopping = stopsInFlight.get(key);
    if (stopping) await stopping.catch(() => undefined);
    const r = rt(key);
    if (r.status === 'starting' || r.status === 'running' || r.status === 'ready') {
      if (target) claimIntent(workspaceId, serviceId, target);
      return r.status;
    }
    const p = startInner(workspaceId, service, target).finally(() => { startsInFlight.delete(key); abortStart.delete(key); });
    startsInFlight.set(key, p);
    return p;
  }

  /** A stop arrived while a start was still working: if the line has not been typed yet,
   *  the start aborts cleanly; if it has, the stop waits for the start to settle and then
   *  proceeds against the pid it recorded. */
  function aborted(key: string): boolean {
    if (!abortStart.has(key)) return false;
    setRt(key, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: 'start cancelled' });
    releasePorts(key, rt(key));
    return true;
  }

  /** A start that gave up before it ever said `starting`: no transition frees what the
   *  resolve booked, so this does. */
  function notStarted(key: string, note: string): ServiceStatus {
    setRt(key, { status: 'stopped', since: null, note });
    releasePorts(key, rt(key));
    return 'stopped';
  }

  async function startInner(workspaceId: string, service: Service, target: string): Promise<ServiceStatus> {
    const serviceId = service.id;
    const key = copyKey(serviceId, target);
    const ws = workspaceOf(workspaceId)!;
    const timer = restartTimers.get(key);
    if (timer) { clearTimeout(timer); restartTimers.delete(key); }

    // 0. What to type, from Rust (docs/hive.md §6.9): the folder re-rooted onto the target's
    //    checkout, placeholders filled from strict charsets, ports booked. A service on its
    //    own folder with no placeholders comes back exactly as defined, without running git,
    //    so `startLine` below types the very line it always has.
    let resolved: ResolvedCopy;
    try {
      resolved = await commands.stackResolveCopy(workspaceId, serviceId, target);
    } catch (e) {
      logWarn(`stack: refused to start ${service.name}${target ? ` on ${target}` : ''}: ${e}`);
      return notStarted(key, `not started — ${e}`);
    }
    setRt(key, { copy: resolved });
    if (aborted(key)) return 'stopped';
    const tabName = copyTabName(service.name, target, resolved.label);

    // 1. A tab to run in: the bound one, else mint one in the background.
    let bound = boundTab(workspaceId, serviceId, target);
    if (!bound) {
      const paneId = ws.active_pane_id ?? ws.panes[0]?.id;
      if (!paneId) { releasePorts(key, rt(key)); throw new Error('Workspace has no pane'); }
      // A Hive's workspace takes no ordinary tab (`hive::refuse_tab_create`), so there the tab is
      // born bound to its service: hidden from its first moment, never active.
      const born = ws.hive ? { service: { id: serviceId, target: target || null } } : {};
      const tab = await workspacesStore.createTab(workspaceId, paneId, tabName, { append: true, background: true, ...born });
      terminalsStore.setSplitContext(tab.id, { cwd: resolved.cwd || null, sshCommand: null, remoteCwd: null });
      await workspacesStore.renameTab(workspaceId, paneId, tab.id, tabName, true);
      await workspacesStore.setTabServiceId(workspaceId, paneId, tab.id, serviceId, target || null);
      bound = boundTab(workspaceId, serviceId, target);
      if (!bound) { releasePorts(key, rt(key)); throw new Error('Could not bind a tab'); }
    } else if (bound.tab.name !== tabName && (target || bound.tab.name.startsWith(`${service.name}@`))) {
      // A one-copy service moved to another checkout keeps its tab (`retarget`); the name
      // follows it. The own folder's tab is never renamed unless a move named it.
      await workspacesStore.renameTab(workspaceId, bound.pane.id, bound.tab.id, tabName, true);
    }
    const tabId = bound.tab.id;
    // Resolved and with a tab: from here a refusal is the shell's (busy, never ran it), which
    // the next launch may well get past. No await between the abort check and the claim, so a
    // stop's `markIntent(false)` lands either before (and aborts this) or after.
    if (target) {
      if (aborted(key)) return 'stopped';
      claimIntent(workspaceId, serviceId, target);
    }

    // 2. Make sure it is mounted (a background workspace's tab may not be) and live.
    const wasLive = !!terminalsStore.get(tabId);
    if (!wasLive) {
      window.dispatchEvent(new CustomEvent('activate-tab', { detail: tabId }));
      await terminalsStore.waitForRegister(tabId, MOUNT_WAIT_MS);
    }
    if (aborted(key)) return 'stopped';
    const instance = terminalsStore.get(tabId);
    if (!instance) return notStarted(key, 'its tab could not be mounted');
    // A fresh run: nothing from a previous one may survive into it — a stale `ptyId` in
    // particular, which `reconcileBindings` would read as "its tab was reloaded" mid-start.
    // `endpointFrom: null` re-arms the output scan: a port belongs to a RUN, so the one
    // carried over from the last one is stale until this run announces its own (§9).
    scanTails.delete(key);
    setRt(key, { status: 'starting', since: Date.now(), stopping: false, note: null, lastExitCode: null, pid: null, ptyId: null, writeAt: null, beganAt: null, noIntegration: false, endpointFrom: null, endpoint: null });

    // 3. The guard. With shell integration, "ready for input" is the shell's own A: a fresh
    //    shell has none until its rc finishes (and its first prompt also emits an
    //    unconditional D;0, which is why exits are gated on B/C below). A live shell must
    //    have had a prompt since its last B/C, or it is mid-command. Facts are per PTY, so
    //    a shell that respawned under this tab id, or a reload's new id, waits for ITS
    //    first prompt like any fresh shell — `wasLive` says nothing about that. Without
    //    integration (no A within the wait) fall back to the tty foreground, which can
    //    only say that no external job holds it.
    const ptyId = instance.ptyId;
    const f = facts(ptyId);
    const promptSince = (t: number | null) => f.lastPromptAt !== null && (t === null || f.lastPromptAt >= t);
    const cancelled = () => abortStart.has(key);
    // With integration switched off no A will ever come; don't wait 12s to learn that. (A
    // PTY reattached after a webview reload is the one case that still pays the wait: its
    // A was emitted before this store existed, and it will not prompt again until Enter.)
    const integrated = preferencesStore.shellIntegration
      ? await waitForFact(ptyId, (x) => x.lastPromptAt !== null || cancelled(), FIRST_PROMPT_WAIT_MS) && !cancelled()
      : false;
    if (aborted(key)) return 'stopped';
    if (integrated) {
      // At a prompt AND settled there: a fresh shell's first A is not its last — bash's
      // PROMPT_COMMAND runs a B/D;0/A cycle of its own right behind it, and a line typed
      // into that gap has the cycle's B taken for our command's and its D;0 for our exit
      // (seen at launch, when every service starts in a just-spawned shell).
      const settled = (x: ShellFacts) => promptSince(x.lastBeginAt) && Date.now() - (x.lastPromptAt ?? 0) >= PROMPT_SETTLE_MS;
      const idle = await waitForFact(ptyId, (x) => settled(x) || cancelled(), 4000);
      if (aborted(key)) return 'stopped';
      if (!idle) {
        const fg = await probeForeground(instance.ptyId);
        const what = fg?.executable ? `${fg.executable} is in the foreground` : 'the shell is mid-command';
        setRt(key, { status: 'stopped', since: null, note: `not started — ${what}` });
        logWarn(`stack: refused to start ${service.name}: ${what}`);
        return 'stopped';
      }
    } else {
      const atPrompt = await waitForPrompt(instance.ptyId, 4000);
      if (!atPrompt) {
        const fg = await probeForeground(instance.ptyId);
        const what = fg?.executable ? `${fg.executable} is in the foreground` : 'the shell is busy';
        setRt(key, { status: 'stopped', since: null, note: `not started — ${what}` });
        logWarn(`stack: refused to start ${service.name}: ${what}`);
        return 'stopped';
      }
      setRt(key, { noIntegration: true });
    }
    if (aborted(key)) return 'stopped';

    // 4. Type it: Rust's folder, command and env for this copy, through the same `startLine`.
    const line = startLine({ cwd: resolved.cwd, env: resolved.env, command: resolved.command });
    setRt(key, { ptyId: instance.ptyId, writeAt: Date.now() });
    await commands.writeTerminal(instance.ptyId, Array.from(new TextEncoder().encode(line + '\n')));
    logInfo(`stack: started ${service.name} in tab ${tabId}`);

    // 5. Confirm the shell took it, then record the job that holds the tty — the pid the
    //    stop guard compares against.
    if (integrated) {
      const began = await waitForFact(ptyId, () => rt(key).beganAt !== null || rt(key).status === 'stopped' || rt(key).status === 'crashed', BEGIN_WAIT_MS);
      const now = rt(key);
      if (now.status === 'stopped' || now.status === 'crashed') return now.status;
      if (!began) {
        // Thirty seconds and no B/C: the shell ate the line (an rc step reading stdin) or
        // is wedged. Disown it completely — a kept `ptyId` cannot help (`onExit` returns
        // on `stopped` first) and poisons the next start. If the line does run later it
        // is unowned; adopting "the first B/C after the fact" would as easily adopt the
        // human's `ls` when they go and look, and then SIGTERM it on Stop.
        setRt(key, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, note: 'not started — the shell never ran the command; check the tab' });
        logWarn(`stack: ${service.name}: no B/C within ${BEGIN_WAIT_MS}ms of the write`);
        return 'stopped';
      }
    } else {
      // No integration: the only evidence is a job taking the tty. Give it a moment; if
      // the foreground never leaves the shell, the command is over (or never ran).
      const took = await (async () => {
        const deadline = Date.now() + 2500;
        while (Date.now() < deadline) {
          const fg = await probeForeground(instance.ptyId);
          if (fg?.shell_at_prompt === false) return true;
          await sleep(250);
        }
        return false;
      })();
      if (!took) { onExit(tabId, ptyId, null); return rt(key).status; }
      setRt(key, { status: 'running', beganAt: Date.now() });
      void watchNoIntegration(key, ptyId);
    }
    for (let i = 0; i < 4; i++) {
      const fg = await probeForeground(instance.ptyId);
      if (fg?.shell_at_prompt === false && fg.pid != null) { setRt(key, { pid: fg.pid }); break; }
      const now = rt(key);
      if (now.status === 'stopped' || now.status === 'crashed') break;
      await sleep(300);
    }
    return rt(key).status;
  }

  /** The no-integration fallback's exit watch: while a service started without OSC 133 is
   *  running, its tty returning to the shell is the only exit signal there is. */
  async function watchNoIntegration(key: string, ptyId: string) {
    const { serviceId, target } = parseCopyKey(key);
    while (true) {
      await sleep(2000);
      const r = rt(key);
      if (!r.noIntegration || r.ptyId !== ptyId || (r.status !== 'running' && r.status !== 'ready')) return;
      const fg = await probeForeground(ptyId);
      if (fg?.shell_at_prompt === true) {
        const bound = [...workspacesStore.workspaces].flatMap((w) => w.panes).flatMap((p) => p.tabs)
          .find((t) => t.service_id === serviceId && (t.service_target ?? '') === target);
        if (bound) onExit(bound.id, ptyId, null);
        return;
      }
    }
  }

  /** The first job leasing any of `keys`, or null. A Rust lookup; an unreachable answer is no
   *  lease, since nothing but the queue can hold one. */
  async function leaseOf(keys: string[]): Promise<StackLeaseState | null> {
    try {
      return (await commands.stackLeaseState(keys)).find((s) => s.job) ?? null;
    } catch (e) {
      logWarn(`stack: lease state for ${keys.join(', ')}: ${e}`);
      return null;
    }
  }

  /** Whether a stop or move here must ask about leases: jobs run only in a Hive, on its own
   *  stack, so any other workspace never asks (and never waits on the asking). */
  function mayBeLeased(workspaceId: string): boolean {
    return !!workspaceOf(workspaceId)?.hive;
  }

  /** Refuse to stop or move a copy a job depends on (docs/hive.md §6.8). */
  async function refuseLeased(service: Service, key: string): Promise<void> {
    const held = await leaseOf([key]);
    if (held) throw new LeaseHeldError(key, held, `${service.name}${parseCopyKey(key).target ? ` on ${labelOf(key)}` : ''}`);
  }

  async function stop(workspaceId: string, serviceId: string, target = ''): Promise<ServiceStatus> {
    const service = serviceOf(workspaceId, serviceId);
    if (!service) throw new Error('Service not found');
    if (service.shared_from) throw new Error(sharedRefusal(service));
    const key = copyKey(serviceId, target);
    // Before the intent is cleared: a refused stop leaves the copy wanted.
    if (mayBeLeased(workspaceId)) await refuseLeased(service, key);
    markIntent(workspaceId, serviceId, target, false);
    const pending = stopsInFlight.get(key);
    if (pending) return pending;
    const starting = startsInFlight.get(key);
    if (starting) {
      // Ask the start to abort if it has not typed yet, then wait for it either way.
      abortStart.add(key);
      await starting.catch(() => undefined);
    }
    const r = rt(key);
    if (r.status === 'stopped') return r.status;
    if (r.status === 'crashed') {
      // Nothing to kill, but a stop is an answer to the crash.
      settleCrash(key);
      return 'stopped';
    }
    const p = stopInner(workspaceId, key, r).finally(() => stopsInFlight.delete(key));
    stopsInFlight.set(key, p);
    return p;
  }

  async function stopInner(workspaceId: string, key: string, r: ServiceRuntime): Promise<ServiceStatus> {
    const { serviceId, target } = parseCopyKey(key);
    const bound = boundTab(workspaceId, serviceId, target);
    const instance = bound ? terminalsStore.get(bound.tab.id) : undefined;
    if (!instance || (r.ptyId && instance.ptyId !== r.ptyId)) {
      // No live shell, or a different one than the command went into: nothing is running.
      setRt(key, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: null });
      return 'stopped';
    }

    // Already back at the prompt: nothing to signal, just record it.
    const fg = await probeForeground(instance.ptyId);
    if (fg?.shell_at_prompt === true) {
      setRt(key, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: null });
      return 'stopped';
    }
    // The guard (§4): only the job we recorded is ever signalled. No recorded pid means we
    // cannot tell whose job holds the terminal — refuse, never fall back to "whatever is in
    // front". A wrong "yes" here ^C's and SIGTERMs the human's vim.
    if (r.pid === null || fg?.pid == null || fg.pid !== r.pid) {
      const what = fg?.executable ? `${fg.executable} is in the foreground` : 'cannot tell which job holds the terminal';
      setRt(key, { note: `not stopped — ${what}` });
      return r.status;
    }
    setRt(key, { stopping: true });

    const ctrlC = [0x03];
    await commands.writeTerminal(instance.ptyId, ctrlC);
    if (await waitForPrompt(instance.ptyId, 3000)) return finishStop(key);
    await commands.writeTerminal(instance.ptyId, ctrlC);
    if (await waitForPrompt(instance.ptyId, 2000)) return finishStop(key);

    await commands.killPtyForegroundJob(instance.ptyId, r.pid, false).catch(() => false);
    if (await waitForPrompt(instance.ptyId, 3000)) return finishStop(key);
    await commands.killPtyForegroundJob(instance.ptyId, r.pid, true).catch(() => false);
    if (await waitForPrompt(instance.ptyId, 2000)) return finishStop(key);

    // Gave up waiting, but the stop was asked for: leave `stopping` set so the exit, when
    // it finally comes, is filed as a stop and not a crash that auto-restarts.
    const after = await probeForeground(instance.ptyId);
    setRt(key, { note: `still stopping — ${after?.executable ?? 'the job'} has not exited yet` });
    return rt(key).status;
  }

  function finishStop(key: string): ServiceStatus {
    // The OSC 133 exit usually lands first and clears `stopping`; this is the fallback
    // for shells without integration.
    if (rt(key).status !== 'stopped') {
      setRt(key, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: null });
    }
    return 'stopped';
  }

  /** Forget a copy's row: it has no tab and no intent any more. The own folder's row stays,
   *  as it always has. */
  function dropRow(key: string) {
    if (!parseCopyKey(key).target || !runtime.has(key)) return;
    const next = new Map(runtime);
    next.delete(key);
    runtime = next;
    scanTails.delete(key);
  }

  /** Move a copy to another checkout (Run on ▸, `restartService { on }`; docs/hive.md §6.9).
   *  A one-copy service (no `{port}`) stops the copy that is up, then starts on the new
   *  checkout IN THE SAME TAB, so moving it back and forth leaves no trail of tabs. A service
   *  that runs copies starts the new one first and stops the old only once the new one is up,
   *  so a failed start leaves the old copy serving. */
  async function retarget(workspaceId: string, serviceId: string, from: string, to: string): Promise<ServiceStatus> {
    const service = serviceOf(workspaceId, serviceId);
    if (!service) throw new Error('Service not found');
    if (service.shared_from) throw new Error(sharedRefusal(service));
    // Up front: a service that runs copies starts the new one before it stops this one.
    if (mayBeLeased(workspaceId)) await refuseLeased(service, copyKey(serviceId, from));
    if (from === to) {
      await stop(workspaceId, serviceId, to);
      return start(workspaceId, serviceId, to);
    }
    let status: ServiceStatus;
    if (!runsManyCopies(service)) {
      const fromKey = copyKey(serviceId, from);
      const after = await stop(workspaceId, serviceId, from);
      if (after !== 'stopped' && after !== 'crashed') {
        throw new Error(`${service.name} on ${labelOf(fromKey)} didn't stop (${rt(fromKey).note ?? after}), so it wasn't moved`);
      }
      const old = boundTab(workspaceId, serviceId, from);
      if (old && !boundTab(workspaceId, serviceId, to)) {
        await workspacesStore.setTabServiceId(workspaceId, old.pane.id, old.tab.id, serviceId, to || null);
        // The drawer follows the tab it was showing to its new copy key.
        followConsole(workspaceId, fromKey, copyKey(serviceId, to));
      }
      dropRow(fromKey);
      status = await start(workspaceId, serviceId, to);
    } else {
      // The limit counts the copy being replaced as gone: it is, once this returns.
      status = await start(workspaceId, serviceId, to, from);
      if (isLive(status)) await stop(workspaceId, serviceId, from);
    }
    commands.stackPruneWorktrees().catch((e) => logWarn(`stack: prune checkouts: ${e}`));
    return status;
  }

  /** Put a service on `to`: Run on ▸ (opened on the row `from`) and `restartService { on }`
   *  (no row) both decide through `moveVerb`, so a crashed copy is moved like a live one. */
  async function runOn(workspaceId: string, serviceId: string, to: string, from: string | null = null): Promise<ServiceStatus> {
    const service = serviceOf(workspaceId, serviceId);
    if (!service) throw new Error('Service not found');
    const copies = copiesOf(workspaceId, serviceId).map((c) => ({ target: c.target, live: c.live, status: c.runtime.status }));
    const m = moveVerb(service, copies, to, from);
    if (m.verb === 'retarget') return retarget(workspaceId, serviceId, m.from, to);
    if (m.verb === 'restart') {
      await stop(workspaceId, serviceId, to);
      return start(workspaceId, serviceId, to);
    }
    return start(workspaceId, serviceId, to);
  }

  function followConsole(workspaceId: string, fromKey: string, toKey: string) {
    if (consoleService.get(workspaceId) === fromKey) consoleService = new Map(consoleService).set(workspaceId, toKey);
    if (consoleVisible.get(workspaceId) === fromKey) consoleVisible = new Map(consoleVisible).set(workspaceId, toKey);
  }

  /** Close a stopped copy on another checkout: its tab goes, with its row and its intent. */
  async function forgetCopy(workspaceId: string, serviceId: string, target: string): Promise<void> {
    if (!target) throw new Error("The service's own folder isn't a copy to close; remove the service instead");
    const key = copyKey(serviceId, target);
    if (isLive(rt(key).status) || startsInFlight.has(key)) throw new Error('Stop that copy before closing it');
    markIntent(workspaceId, serviceId, target, false);
    releasePorts(key, rt(key));
    const bound = boundTab(workspaceId, serviceId, target);
    if (bound) await workspacesStore.deleteTab(workspaceId, bound.pane.id, bound.tab.id);
    dropRow(key);
    for (const [ws, k] of consoleService) if (ws === workspaceId && k === key) closeConsoleOf(workspaceId);
    commands.stackPruneWorktrees().catch((e) => logWarn(`stack: prune checkouts: ${e}`));
  }

  function closeConsoleOf(workspaceId: string) {
    if (!consoleService.has(workspaceId)) return;
    const next = new Map(consoleService);
    next.delete(workspaceId);
    consoleService = next;
    // Closed now; still on screen until the drawer has finished sliding out. Reopening
    // inside the window goes through viewConsole, which cancels this.
    clearTimeout(consoleCloseTimers.get(workspaceId));
    consoleCloseTimers.set(
      workspaceId,
      setTimeout(() => {
        consoleCloseTimers.delete(workspaceId);
        if (consoleService.has(workspaceId)) return;
        const shown = new Map(consoleVisible);
        shown.delete(workspaceId);
        consoleVisible = shown;
      }, CONSOLE_CLOSE_MS),
    );
  }

  // ── Run-queue jobs (docs/hive.md §6.8) ──────────────────────────────────────────

  /** `stack.job.start`: Rust has inserted the job's row and built its line; mint the hidden tab
   *  bound to it, wait for its shell's prompt through the same guard as a service, arm the job in
   *  Rust, type the line, and confirm once the shell's own B for it is seen. A shell with no
   *  OSC 133 refuses the job: its exit could never be read, so it could never be a receipt. */
  async function startJob(workspaceId: string, jobId: string, row: Service, line: string): Promise<JobStartAnswer> {
    const ws = workspaceOf(workspaceId);
    if (!ws?.hive) return { error: 'This window holds no such Hive.' };
    if (ws.suspended) return { error: 'The workspace is suspended.' };
    if (!preferencesStore.shellIntegration) return { accepted: false, error: NEEDS_INTEGRATION };
    if (jobRows.has(jobId)) return { error: `Job ${jobId} was already started here.` };
    const paneId = ws.active_pane_id ?? ws.panes[0]?.id;
    if (!paneId) return { error: 'The workspace has no pane.' };
    jobRows.set(jobId, { workspaceId, row });
    jobKeys.add(jobId);
    const key = jobId;
    let tab: Tab;
    try {
      tab = await workspacesStore.createTab(workspaceId, paneId, row.name, { append: true, background: true, service: { id: jobId } });
    } catch (e) {
      forgetJob(jobId);
      return { error: `its tab could not be made: ${e}` };
    }
    terminalsStore.setSplitContext(tab.id, { cwd: row.cwd || null, sshCommand: null, remoteCwd: null });
    await workspacesStore.renameTab(workspaceId, paneId, tab.id, row.name, true);
    window.dispatchEvent(new CustomEvent('activate-tab', { detail: tab.id }));
    // Before the line is typed, a refused start closes the tab it made: Rust may have given the
    // job up already (released mid-start, or its rpc timed out) and never learns this tab's id.
    const refuse = async (error: string, ptyId?: string): Promise<JobStartAnswer> => {
      logWarn(`stack: job ${jobId} not started: ${error}`);
      await workspacesStore.deleteTab(workspaceId, paneId, tab.id).catch((e) => logWarn(`stack: close job tab ${tab.id}: ${e}`));
      forgetJob(jobId);
      return { accepted: false, error, tab_id: tab.id, pty_id: ptyId };
    };
    await terminalsStore.waitForRegister(tab.id, MOUNT_WAIT_MS);
    const instance = terminalsStore.get(tab.id);
    if (!instance) return refuse('its tab could not be mounted');
    const ptyId = instance.ptyId;
    setRt(key, { status: 'starting', since: Date.now(), stopping: false, note: null, lastExitCode: null, pid: null, ptyId: null, writeAt: null, beganAt: null, noIntegration: false, endpointFrom: null, endpoint: null });
    if (!(await waitForFact(ptyId, (x) => x.lastPromptAt !== null, FIRST_PROMPT_WAIT_MS))) return refuse(NEEDS_INTEGRATION, ptyId);
    // At its prompt and settled there, as `startInner` waits (bash's own B/D;0/A cycle).
    const settled = (x: ShellFacts) => x.lastPromptAt !== null && (x.lastBeginAt === null || x.lastPromptAt >= x.lastBeginAt) && Date.now() - x.lastPromptAt >= PROMPT_SETTLE_MS;
    if (!(await waitForFact(ptyId, settled, 4000))) return refuse('the shell is mid-command', ptyId);
    // Rust refuses a job it has given up on, so a window that woke late types nothing.
    try {
      await commands.stackJobArm(jobId, tab.id, ptyId);
    } catch (e) {
      return refuse(String(e), ptyId);
    }
    setRt(key, { ptyId, writeAt: Date.now() });
    await commands.writeTerminal(ptyId, Array.from(new TextEncoder().encode(line + '\n')));
    logInfo(`stack: job ${jobId} typed in tab ${tab.id}`);
    await waitForFact(ptyId, () => rt(key).beganAt !== null || rt(key).status === 'stopped', BEGIN_WAIT_MS);
    const confirmed = rt(key).beganAt !== null;
    if (!confirmed) setRt(key, { status: 'stopped', since: null, note: 'not started — the shell never ran the line; check the tab' });
    // The job that holds the tty: the pid `stopJob`'s guard compares against.
    for (let i = 0; confirmed && i < 4 && isLive(rt(key).status); i++) {
      const fg = await probeForeground(ptyId);
      if (fg?.shell_at_prompt === false && fg.pid != null) { setRt(key, { pid: fg.pid }); break; }
      await sleep(300);
    }
    return { accepted: true, confirmed, tab_id: tab.id, pty_id: ptyId };
  }

  /** A job's row and runtime go: its tab is closed, or was never kept. */
  function forgetJob(jobId: string) {
    jobRows.delete(jobId);
    if (runtime.has(jobId)) {
      const next = new Map(runtime);
      next.delete(jobId);
      runtime = next;
    }
  }

  /** `stack.job.stop`: ^C under the pid guard (a timeout, a release, Stop anyway). Never intent.
   *  Nothing cached is trusted: a job can print a D of its own (its row then reads over while it
   *  runs), and a window reload empties `jobRows` while the job's PTY lives on. Its tab is found
   *  by its binding and the terminal says whether the job is still in front. A stop that didn't
   *  land answers `{error}`, never a quiet success. */
  async function stopJob(workspaceId: string, jobId: string): Promise<{ status: ServiceStatus } | { error: string }> {
    const ws = workspaceOf(workspaceId);
    if (!ws) return { error: 'This window holds no such workspace.' };
    const bound = boundIn(ws, jobId, '');
    // No tab: its PTY closed with it, and the job with that.
    if (!bound) { forgetJob(jobId); return { status: 'stopped' }; }
    const instance = terminalsStore.get(bound.tab.id);
    if (!instance) return { error: `job ${jobId}'s terminal isn't reachable in this window` };
    if (!jobRows.has(jobId)) adoptJob(ws, jobId);
    const pending = stopsInFlight.get(jobId);
    const status = pending ? await pending : await (async () => {
      const fg = await probeForeground(instance.ptyId);
      if (fg?.shell_at_prompt === true) return finishStop(jobId);
      // A reload left no pid here. The tab is the job's own, minted for its one line and only
      // ever stopped while Rust holds the job running, so what holds its terminal is the job.
      if (rt(jobId).ptyId !== instance.ptyId || rt(jobId).pid === null) {
        if (fg?.pid == null) return null;
        setRt(jobId, { ptyId: instance.ptyId, pid: fg.pid });
      }
      if (!isLive(rt(jobId).status)) setRt(jobId, { status: 'running' });
      const p = stopInner(workspaceId, jobId, rt(jobId)).finally(() => stopsInFlight.delete(jobId));
      stopsInFlight.set(jobId, p);
      return p;
    })();
    if (status === null) return { error: `cannot tell which process holds job ${jobId}'s terminal` };
    return status === 'stopped' ? { status } : { error: `job ${jobId} not stopped: ${rt(jobId).note ?? status}` };
  }

  /** `stack.job.cleanup`: a worker's finished job tabs close, by tab or by job. One whose job
   *  still holds its terminal is left alone, unless `force` (the worker retired, its jobs were
   *  interrupted, and closing the tab ends what's left). */
  async function cleanupJobs(workspaceId: string, tabIds: string[], jobIds: string[], force = false): Promise<number> {
    const ws = workspaceOf(workspaceId);
    if (!ws) return 0;
    let closed = 0;
    for (const pane of ws.panes) {
      for (const tab of [...pane.tabs]) {
        const sid = tab.service_id ?? '';
        if (!(tabIds.includes(tab.id) || jobIds.includes(sid))) continue;
        // Only a job's tab: a service's is never closed by the queue.
        if (!jobKeys.has(sid) && !/^j-[0-9a-f]{8}$/.test(sid)) continue;
        if (!force && isLive(rt(sid).status)) {
          // Live here may only mean its exit wasn't confirmed: the terminal decides.
          const instance = terminalsStore.get(tab.id);
          if (instance && (await probeForeground(instance.ptyId))?.shell_at_prompt !== true) continue;
        }
        await workspacesStore.deleteTab(workspaceId, pane.id, tab.id).catch((e) => logWarn(`stack: close job tab ${tab.id}: ${e}`));
        forgetJob(sid);
        closed++;
      }
    }
    return closed;
  }

  /** A stop, restart or move of one copy, as a held action names it. */
  async function act(workspaceId: string, serviceId: string, target: string, action: string, to: string | null): Promise<ServiceStatus> {
    if (action === 'stop') return stop(workspaceId, serviceId, target);
    if (action === 'retarget' && to != null) return runOn(workspaceId, serviceId, to, target);
    await stop(workspaceId, serviceId, target);
    return start(workspaceId, serviceId, target);
  }

  /** Rust says a copy's lease ended: carry out what waited for it (Run when free, or an agent's
   *  stop or move queued behind the job). Rust leases the copy to no new run until this says
   *  done; should one have it anyway, the action waits behind that run, never dropped. */
  async function onLeaseFreed(h: StackHeldAction) {
    const { serviceId, target } = parseCopyKey(h.copy_key);
    try {
      if (!serviceOf(h.workspace_id, serviceId)) return;
      logInfo(`stack: ${h.copy_key} is free; ${h.action}${h.to != null ? ` to ${h.to}` : ''} (${h.by}, after ${h.job})`);
      await act(h.workspace_id, serviceId, target, h.action, h.to ?? null);
    } catch (e) {
      if (e instanceof LeaseHeldError && (h.action === 'stop' || h.action === 'restart' || h.action === 'retarget')) {
        await commands
          .stackLeaseHold(h.workspace_id, h.copy_key, h.action, h.to ?? null, h.by === 'agent' ? 'agent' : 'human')
          .then((again) => logInfo(`stack: ${h.action} of ${h.copy_key} waits again, behind ${again.job}`))
          .catch((err) => logError(`stack: ${h.action} of ${h.copy_key} lost behind a new run: ${err}`));
      } else {
        logError(`stack: ${h.action} of ${h.copy_key} after its lease: ${e}`);
      }
    } finally {
      await commands.stackLeaseDone(h.copy_key).catch((e) => logWarn(`stack: lease of ${h.copy_key} not handed back: ${e}`));
    }
  }

  /** Read every `shared_from` service's source from the Rust mirror, which holds every
   *  window's stack (docs/hive.md §6.8). Only what changed is written back. */
  async function pollShared() {
    const refs = workspacesStore.workspaces.flatMap((w) => (w.stack ?? []).filter((s) => s.shared_from).map((s) => s));
    if (refs.length === 0 && shared.size === 0) return;
    const next = new Map<string, StackSharedRow | null>();
    for (const s of refs) {
      const ref = s.shared_from!;
      next.set(s.id, await commands.getStackRuntimeRow(ref.workspace_id, ref.service_id).catch(() => null));
    }
    if (JSON.stringify([...next]) !== JSON.stringify([...shared])) shared = next;
  }

  return {
    /** Runtime state for a copy (idle defaults when never started). The key is the copy key:
     *  a service id alone is its own folder's copy. */
    runtime(key: string): ServiceRuntime { return rt(key); },
    status(key: string): ServiceStatus { return rt(key).status; },
    /** Hear every status change (`to` is the runtime as it now stands). Returns the unsubscribe. */
    onTransition(fn: (key: string, from: ServiceStatus, to: ServiceRuntime) => void): () => void {
      transitionListeners.add(fn);
      return () => { transitionListeners.delete(fn); };
    },
    boundTab,
    serviceForTab,
    /** Every copy of a service worth showing, the own folder's first (docs/hive.md §6.9). */
    copies: copiesOf,
    /** The copies starting or up. */
    liveCopies,
    /** Any copy of any service in the workspace starting or up. */
    anyLive(workspaceId: string): boolean {
      return (workspaceOf(workspaceId)?.stack ?? []).some((s) => liveCopies(workspaceId, s.id).length > 0);
    },
    /** A `shared_from` service's source as the Rust mirror last said: undefined before the
     *  first read, null when the source is gone. */
    sharedStatus(serviceId: string): StackSharedRow | null | undefined {
      return shared.get(serviceId);
    },
    /** Where a copy serves: the definition's persisted endpoint for the own folder (as always),
     *  else what this run of the copy announced, else the port Rust booked for it. */
    endpoint(workspaceId: string, serviceId: string, target = ''): { port: number | null; url: string | null } {
      const service = serviceOf(workspaceId, serviceId);
      if (!target) return { port: service?.port ?? null, url: service?.url ?? null };
      const r = rt(copyKey(serviceId, target));
      return r.endpoint ?? { port: r.copy?.port ?? null, url: null };
    },

    // ── The console drawer (docs/stack.md §7) ────────────────────────────────────
    // A service tab is never in the tab strip and is never a pane's active tab; this is
    // the only way to see one. The terminal itself does not move panes — it portals into
    // the drawer's slot, which is why opening the drawer resizes nothing else.

    /** The copy (its copy key; a bare service id is the own folder's) this workspace's drawer
     *  is OPEN on, or null the instant it is closed. Everything that asks "is the console
     *  open" wants this one. */
    consoleServiceId(workspaceId: string): string | null {
      return consoleService.get(workspaceId) ?? null;
    },

    /** The copy still ON SCREEN, which outlasts the close by the slide-out. The drawer
     *  renders from this so it keeps its contents while it goes. */
    consoleVisibleServiceId(workspaceId: string): string | null {
      return consoleVisible.get(workspaceId) ?? null;
    },

    /** The tab whose terminal the drawer is showing — what `+page` makes visible. Lags the
     *  close with the drawer: hide the terminal at t=0 and an empty box slides out. */
    consoleTabId(workspaceId: string): string | null {
      const key = consoleVisible.get(workspaceId);
      if (!key) return null;
      const { serviceId, target } = parseCopyKey(key);
      return boundTab(workspaceId, serviceId, target)?.tab.id ?? null;
    },

    /** Show a copy (by copy key). Mounts its tab if it is not mounted yet (a service started
     *  in an earlier session has a tab but no pane to mount it), so the terminal can portal in. */
    viewConsole(workspaceId: string, key: string) {
      const timer = consoleCloseTimers.get(workspaceId);
      if (timer) {
        clearTimeout(timer);
        consoleCloseTimers.delete(workspaceId);
      }
      const next = new Map(consoleService);
      next.set(workspaceId, key);
      consoleService = next;
      const shown = new Map(consoleVisible);
      shown.set(workspaceId, key);
      consoleVisible = shown;
      const { serviceId, target } = parseCopyKey(key);
      const bound = boundTab(workspaceId, serviceId, target);
      // `activate-tab` carries a bare tab id — an object detail is silently ignored by the
      // listener in `+page`, and the drawer then shows an empty slot forever.
      if (bound && !terminalsStore.get(bound.tab.id)) {
        window.dispatchEvent(new CustomEvent('activate-tab', { detail: bound.tab.id }));
      }
    },

    closeConsole(workspaceId: string) {
      closeConsoleOf(workspaceId);
    },

    toggleConsole(workspaceId: string, key: string) {
      if (consoleService.get(workspaceId) === key) this.closeConsole(workspaceId);
      else this.viewConsole(workspaceId, key);
    },

    services(workspaceId: string): Service[] {
      // A job's row never reaches the mirror; the filter only guards a state loaded mid-run.
      const all = workspaceOf(workspaceId)?.stack ?? [];
      return all.some((s) => s.kind === 'job') ? all.filter((s) => s.kind !== 'job') : all;
    },

    /** Rollup for the sidebar dot: null when the stack is empty or all stopped. Each service's
     *  own folder counts as it always has; a copy elsewhere counts only while it is up (it is
     *  never "expected", so a stopped one can't make the dot partial). A shared service is
     *  another workspace's to report. */
    rollup(workspaceId: string): Rollup {
      const rows: { status: ServiceStatus; autoStart: boolean }[] = [];
      for (const s of this.services(workspaceId)) {
        if (s.shared_from) continue;
        rows.push({ status: rt(s.id).status, autoStart: s.auto_start ?? true });
        for (const c of copiesOf(workspaceId, s.id)) {
          if (c.target && c.runtime.status !== 'stopped') rows.push({ status: c.runtime.status, autoStart: false });
        }
      }
      return rollupStatus(rows);
    },

    // ── Definitions ─────────────────────────────────────────────────────────────

    /** Idempotent by normalized name (returns the existing service), like createTasks. */
    async createService(workspaceId: string, input: ServiceInput): Promise<Service> {
      const ws = workspaceOf(workspaceId);
      if (!ws) throw new Error('Workspace not found');
      const name = input.name.trim();
      if (!name) throw new Error('A service needs a name');
      const sharedFrom = input.shared_from ?? null;
      if (sharedFrom && sharedFrom.workspace_id === workspaceId) throw new Error('A service can only be shared from another workspace');
      if (!sharedFrom && !input.command.trim()) throw new Error('A service needs a command');
      if (checkoutInCommand(input.command)) throw new Error('{checkout} can only be used in env values: a path with spaces in it isn\'t safe in a command');
      const key = normalizeTitle(name);
      const existing = (ws.stack ?? []).find((s) => s.normalized_name === key);
      if (existing) return existing;
      const now = new Date().toISOString();
      const service: Service = {
        id: crypto.randomUUID(),
        name,
        normalized_name: key,
        command: input.command.trim(),
        cwd: input.cwd,
        env: input.env ?? [],
        ssh_command: null,
        auto_start: input.auto_start ?? true,
        restart: input.restart ?? 'on_crash',
        ready_pattern: input.ready_pattern ?? null,
        port: null,
        url: null,
        origin: input.origin ?? 'human',
        // `createService` over MCP: an agent wrote every field of it.
        agent_edited: input.origin === 'agent',
        ...(sharedFrom ? { shared_from: sharedFrom } : {}),
        created_at: now,
        updated_at: now,
      };
      await persist(workspaceId, [...(ws.stack ?? []), service]);
      // The stored row, not the draft: Rust expanded `~` in cwd on the way in.
      return workspaceOf(workspaceId)?.stack?.find((s) => s.id === service.id) ?? service;
    },

    async updateService(workspaceId: string, serviceId: string, patch: Partial<Omit<Service, 'id' | 'created_at' | 'normalized_name'>>): Promise<Service> {
      const ws = workspaceOf(workspaceId);
      const current = ws?.stack?.find((s) => s.id === serviceId);
      if (!ws || !current) throw new Error('Service not found');
      if (patch.command !== undefined && checkoutInCommand(patch.command)) {
        throw new Error('{checkout} can only be used in env values: a path with spaces in it isn\'t safe in a command');
      }
      const next: Service = { ...current, ...patch, updated_at: new Date().toISOString() };
      if (patch.name !== undefined) {
        next.name = patch.name.trim();
        next.normalized_name = normalizeTitle(next.name);
      }
      await persist(workspaceId, (ws.stack ?? []).map((s) => (s.id === serviceId ? next : s)));
      return workspaceOf(workspaceId)?.stack?.find((s) => s.id === serviceId) ?? next;
    },

    /** Refused while any copy runs — stop first (the caller decides; nothing here types ^C).
     *  Every copy's tab goes back to the strip as an ordinary terminal holding its output. */
    async removeService(workspaceId: string, serviceId: string): Promise<void> {
      const ws = workspaceOf(workspaceId);
      if (!ws) throw new Error('Workspace not found');
      const copies = copiesOf(workspaceId, serviceId);
      if (copies.some((c) => c.live)) {
        throw new Error('Stop the service before removing it');
      }
      for (const c of copies) {
        const bound = boundTab(workspaceId, serviceId, c.target);
        if (bound) await workspacesStore.setTabServiceId(workspaceId, bound.pane.id, bound.tab.id, null);
        releasePorts(c.key, c.runtime);
      }
      const next = new Map(runtime);
      for (const c of copies) next.delete(c.key);
      runtime = next;
      await persist(workspaceId, (ws.stack ?? []).filter((s) => s.id !== serviceId));
    },

    /** An agent, or maiTerm's own output scan, reporting what it saw (§6.1, §9). `from`
     *  records which — an agent's report is hearsay maiTerm passes on, an observation is
     *  maiTerm reading the service's own words. A copy on another checkout keeps its address
     *  in its runtime: the definition's `port`/`url` are the own folder's. */
    async reportEndpoint(workspaceId: string, serviceId: string, report: { port?: number | null; url?: string | null; ready?: boolean; note?: string | null }, from: 'observed' | 'reported' = 'reported', target = '') {
      const key = copyKey(serviceId, target);
      const patch: Partial<Service> = {};
      if (report.port !== undefined) patch.port = report.port;
      if (report.url !== undefined) patch.url = report.url;
      if (Object.keys(patch).length) {
        if (target) {
          const cur = rt(key).endpoint ?? { port: null, url: null };
          setRt(key, { endpoint: { port: patch.port !== undefined ? patch.port ?? null : cur.port, url: patch.url !== undefined ? patch.url ?? null : cur.url } });
        } else {
          await this.updateService(workspaceId, serviceId, patch);
        }
        // Provenance belongs to an endpoint that EXISTS. An agent clearing a stale value
        // (`updateService { port: null }`, which the tool description invites) is removing
        // a wrong answer, not supplying a right one — recording it as provenance would
        // disarm `observeOutput` for the rest of the run and leave the service with no
        // address at all. Clearing re-arms the scan instead.
        const establishes = report.port != null || report.url != null;
        setRt(key, { endpointFrom: establishes ? from : null });
      }
      const r = rt(key);
      if (report.ready && (r.status === 'starting' || r.status === 'running')) setRt(key, { status: 'ready' });
      if (report.note !== undefined) setRt(key, { note: report.note });
    },

    /** Every PTY chunk out of a service tab. maiTerm reads the service's own output for
     *  the address it is serving on, so a port reaches the sidebar with no agent in the
     *  loop — before this, `updateService` was the ONLY way one ever got there.
     *
     *  Bounded by design: the scan stops at the first endpoint of a run, so the cost is a
     *  regex over the first seconds of a start and nothing whatsoever after that. An agent
     *  can still correct what we read; `updateService` simply overwrites it. */
    observeOutput(tabId: string, data: Uint8Array) {
      const found = serviceForTab(tabId);
      if (!found) return;
      const { workspaceId, service, target, key } = found;
      const r = rt(key);
      // Stop at the first answer of a run: `ready` is the destination either route reaches.
      if (r.endpointFrom) return;
      if (r.status !== 'starting' && r.status !== 'running') return;
      // Nothing before the command BEGAN is the command talking. The pty echoes the line
      // maiTerm typed, so without this the scan matches the start line against itself —
      // `uvicorn app:app --port 8000` announcing port 8000 before uvicorn has run at all.
      // Same doctrine as every other write in this store: sequence on the shell's own
      // OSC 133, never on "output arrived". It also keeps `onCommandBegin`'s
      // `status === 'starting'` guard reachable, which a premature flip to `ready` would
      // consume — and that would strand the service as "the shell never ran the command".
      if (!r.beganAt) return;
      // And stop looking eventually. A service that announces itself does so while it is
      // starting; a chatty worker that never will should not be scanned for its whole life.
      if (r.since && Date.now() - r.since > SCAN_WINDOW_MS) return;

      const probe = (scanTails.get(key) ?? '') + stripAnsi(new TextDecoder().decode(data)).replace(/\r/g, '');

      // A human-supplied `ready_pattern` overrides the built-in shapes — that is the whole
      // point of the field. It carries readiness; the address comes from its optional
      // `port` group, or failing that from whatever the built-in can still find.
      const pattern = readyPattern(service);
      if (pattern) {
        const m = pattern.exec(probe);
        if (!m) { scanTails.set(key, probe.slice(-SCAN_TAIL_CHARS)); return; }
        scanTails.delete(key);
        const grouped = m.groups?.port ? Number(m.groups.port) : null;
        const fallback = grouped == null ? detectEndpoint(probe) : null;
        const port = grouped ?? fallback?.port ?? null;
        void this.reportEndpoint(
          workspaceId, service.id,
          port == null ? { ready: true } : { port, url: fallback?.url ?? null, ready: true },
          'observed', target,
        ).catch((e) => logError(`stack: ready pattern for ${service.name}: ${e}`));
        return;
      }

      const hit = detectEndpoint(probe);
      if (!hit) {
        scanTails.set(key, probe.slice(-SCAN_TAIL_CHARS));
        return;
      }
      scanTails.delete(key);
      // `url` is passed even when null: it CLEARS a launchable url left over from a run
      // that served on a different port, which would otherwise outlive its port.
      void this.reportEndpoint(workspaceId, service.id, { port: hit.port, url: hit.url, ready: true }, 'observed', target)
        .catch((e) => logError(`stack: endpoint for ${service.name}: ${e}`));
    },

    // ── Verbs ────────────────────────────────────────────────────────────────────
    // Each takes the copy's target last, defaulting to the own folder: a caller that never
    // heard of copies acts on the service exactly as before.

    start,
    stop,
    async restart(workspaceId: string, serviceId: string, target = ''): Promise<ServiceStatus> {
      await stop(workspaceId, serviceId, target);
      return start(workspaceId, serviceId, target);
    },
    retarget,
    runOn,
    forgetCopy,

    /** Run when free (docs/hive.md §6.8): the stop, restart or move waits in Rust until no job
     *  leases the copy, and runs here when `stack-lease-freed` says so. */
    runWhenFree(workspaceId: string, serviceId: string, target: string, action: 'stop' | 'restart' | 'retarget', to: string | null = null, by: 'human' | 'agent' = 'human'): Promise<StackHeldAction> {
      return commands.stackLeaseHold(workspaceId, copyKey(serviceId, target), action, to, by);
    },
    /** Stop anyway: every job leasing the copy is interrupted (never a receipt), then the stop,
     *  restart or move goes ahead. Rust leases the copy to no new run until it has. */
    async stopAnyway(workspaceId: string, serviceId: string, target = '', action: 'stop' | 'restart' | 'retarget' = 'stop', to: string | null = null): Promise<ServiceStatus> {
      const key = copyKey(serviceId, target);
      await commands.hiveLeaseOverride(key);
      try {
        return await act(workspaceId, serviceId, target, action, to);
      } finally {
        await commands.stackLeaseDone(key).catch((e) => logWarn(`stack: lease of ${key} not handed back: ${e}`));
      }
    },
    /** The job leasing a copy now, or null. */
    leaseOf(workspaceId: string, serviceId: string, target = ''): Promise<StackLeaseState | null> {
      return mayBeLeased(workspaceId) ? leaseOf([copyKey(serviceId, target)]) : Promise.resolve(null);
    },

    // The run queue's window half (`hive/rpc.ts`).
    startJob,
    stopJob,
    cleanupJobs,
    forgetJob,

    /** Every `auto_start` service's own folder, serially, in list order. */
    async startStack(workspaceId: string): Promise<void> {
      for (const s of this.services(workspaceId)) {
        if (!(s.auto_start ?? true) || s.shared_from) continue;
        try { await start(workspaceId, s.id); } catch (e) { logError(`stack: start ${s.name}: ${e}`); }
      }
    },

    /** Every copy that isn't stopped, of every service. */
    async stopStack(workspaceId: string): Promise<void> {
      for (const s of this.services(workspaceId)) {
        if (s.shared_from) continue;
        for (const c of copiesOf(workspaceId, s.id)) {
          if (rt(c.key).status === 'stopped') continue;
          // Crashed ones too: a pending auto-restart must not outlive "Stop stack".
          try { await stop(workspaceId, s.id, c.target); } catch (e) { logError(`stack: stop ${s.name}: ${e}`); }
        }
      }
    },

    /** A workspace is leaving for another window with its service tabs still running. Hand
     *  over their runtime and forget it here BEFORE the workspace leaves this window's list:
     *  otherwise `reconcileBindings` finds no tab and files every running service `crashed`
     *  — and publishes that into the Rust mirror the other window reads (docs/stack.md §6.2). */
    takeRuntime(workspaceId: string): Array<[string, ServiceRuntime]> {
      const ids = new Set((workspaceOf(workspaceId)?.stack ?? []).map((s) => s.id));
      const keys = new Set([...ids, ...[...runtime.keys()].filter((k) => ids.has(parseCopyKey(k).serviceId))]);
      const out: Array<[string, ServiceRuntime]> = [];
      const next = new Map(runtime);
      for (const key of keys) {
        const r = runtime.get(key);
        if (r) out.push([key, r]);
        next.delete(key);
        clearTimeout(restartTimers.get(key));
        restartTimers.delete(key);
        scanTails.delete(key);
      }
      runtime = next;
      consoleService.delete(workspaceId);
      consoleService = new Map(consoleService);
      consoleVisible.delete(workspaceId);
      consoleVisible = new Map(consoleVisible);
      autoStarted.delete(workspaceId);
      return out;
    },

    /** Arriving half of `takeRuntime`. Auto-start must be held BEFORE the workspace becomes
     *  active here (`holdAutoStart`), and the rows adopted only once its service tabs have
     *  reattached — a row whose `ptyId` has no live instance yet reads as a reload. */
    holdAutoStart(workspaceId: string) {
      autoStarted.add(workspaceId);
    },
    adoptRuntime(rows: Array<[string, ServiceRuntime]>) {
      if (rows.length === 0) return;
      const next = new Map(runtime);
      for (const [id, r] of rows) next.set(id, r);
      runtime = next;
    },

    /** Fires once per workspace activation (§5); re-armed by suspend. Starts the
     *  `auto_start` services and every one that was left running — each copy on its own
     *  checkout (`running_targets`); `auto_start` is the own folder's alone, and only while
     *  the copies left running leave it room (`restorePlan`). */
    async autoStart(workspaceId: string): Promise<void> {
      const ws = workspaceOf(workspaceId);
      if (!loaded || !ws || ws.suspended || autoStarted.has(workspaceId)) return;
      const plan = (s: Service) => restorePlan(s, 'activation', (t) => survivedReload(workspaceId, s.id, t));
      if (!(ws.stack ?? []).some((s) => plan(s).length > 0)) return;
      autoStarted.add(workspaceId);
      for (const s of this.services(workspaceId)) {
        for (const t of plan(s)) {
          try { await start(workspaceId, s.id, t); } catch (e) { logError(`stack: start ${s.name}${t ? ` on ${t}` : ''}: ${e}`); }
        }
      }
    },

    /** Once `load()` has resolved: the active workspace's auto-start, held until now, and
     *  what was running when the app quit, in EVERY workspace of this window — a service
     *  left running in a background workspace was running, whether or not anyone looks at
     *  it, and each copy comes back on its own checkout. `auto_start` alone still waits for
     *  activation (§12.4). */
    async onLoaded(): Promise<void> {
      loaded = true;
      const active = workspacesStore.activeWorkspaceId;
      if (active) await this.autoStart(active).catch((e) => logError(`stack: auto-start ${active}: ${e}`));
      for (const ws of workspacesStore.workspaces) {
        if (ws.suspended) continue;
        for (const s of ws.stack ?? []) {
          for (const t of restorePlan(s, 'launch', (x) => survivedReload(ws.id, s.id, x))) {
            try { await start(ws.id, s.id, t); } catch (e) { logError(`stack: resume ${s.name}${t ? ` on ${t}` : ''}: ${e}`); }
          }
        }
      }
    },

    /** Resolve when the service is READY — it announced where it is serving — or when it
     *  reaches a terminal state, else after `timeoutMs` with whatever the status is then.
     *
     *  It deliberately does NOT settle for `running`, which used to be the target for a
     *  service with no `ready_pattern`. `running` is set at `onCommandBegin`, the instant
     *  the shell reports the command started — for `npm run dev` that is well before Vite
     *  binds a port, so a caller that waited and then hit the service got ECONNREFUSED,
     *  the exact failure this exists to prevent. Now that `observeOutput` gives every
     *  service a route to `ready` (§9), waiting for it is both possible and right.
     *
     *  The cost lands on a service that never announces anything — a queue worker — which
     *  burns the caller's whole timeout and returns `running`. That is the better error of
     *  the two: waiting too long is an annoyance, answering "up" too early is a wrong
     *  answer. Callers who know they are waiting on such a service pass a short timeout. */
    async waitFor(_workspaceId: string, key: string, timeoutMs: number): Promise<ServiceStatus> {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        const st = rt(key).status;
        if (st === 'ready' || st === 'crashed' || st === 'stopped') return st;
        await sleep(250);
      }
      return rt(key).status;
    },

    // ── Lifecycle ────────────────────────────────────────────────────────────────

    init() {
      // The raw OSC 133 feed — `onCommandComplete` hides exits inside the pane's 2s mount
      // window and under its 2s completion floor, which is precisely where a service that
      // fails at boot exits. Everything is sequenced against our own write: a D counts
      // only after our command's B/C, and a fresh shell is not written to before its A.
      unsubscribe.push(activityStore.onShellPrompt(onShellPrompt));
      unsubscribe.push(activityStore.onCommandBegin(onCommandBegin));
      unsubscribe.push(activityStore.onCommandExit(onExit));
      const self = this;
      unsubscribe.push($effect.root(() => {
        $effect(() => {
          // Subscribe to tab membership, suspend flags and PTY (re)registrations only; the
          // reconcilers read and write `runtime`, which must not re-trigger this effect
          // (CLAUDE.md: untrack).
          void workspacesStore.workspaces.map((w) => [w.suspended, ...w.panes.map((p) => p.tabs.map((t) => `${t.service_id}:${t.service_target}:${t.suspended_at}`).join(','))]);
          void terminalsStore.instanceVersion;
          untrack(() => { reconcileBindings(); reconcileSuspended(); });
        });
        $effect(() => {
          const id = workspacesStore.activeWorkspaceId;
          if (id) untrack(() => { void self.autoStart(id).catch((e) => logError(`stack: auto-start ${id}: ${e}`)); });
        });
        // Mirror the runtime snapshot into Rust so the SessionStart priming can say what is
        // up without a webview round trip (docs/stack.md §6.2). Whole map each time — it is
        // a handful of rows, and a diff would be the fourth place status is bookkept.
        // Keyed by copy key, as Rust keys it (`stack::copy_key`). The own folder's row is
        // exactly what it was before copies existed; another copy's also says where it runs.
        let published = new Set<string>();
        $effect(() => {
          // A job is the run queue's to report, never a service the priming offers.
          const rows = [...runtime.entries()].filter(([key]) => !jobKeys.has(key)).map(([key, r]) => [key, publishRow(key, r)] as const);
          const keys = new Set(rows.map(([key]) => key));
          const removed = [...published].filter((key) => !keys.has(key));
          published = keys;
          commands.publishStackRuntime(rows.map(([, row]) => row), removed).catch((e) => logError(`stack: publish runtime: ${e}`));
        });
      }));
      // A shared service's status lives in another workspace — maybe another window — so it
      // is read from the Rust mirror on a slow tick rather than kept here (docs/hive.md §6.8).
      void pollShared();
      sharedTimer = setInterval(() => { void pollShared(); }, SHARED_POLL_MS);
      // A copy a job leased is free: what waited for it runs (docs/hive.md §6.8). Sent to this
      // window only (`emit_to`).
      getCurrentWindow()
        .listen<StackHeldAction>('stack-lease-freed', (e) => { void onLeaseFreed(e.payload); })
        .then((un) => { unsubscribe.push(un); })
        .catch((e) => logError(`stack: listen for freed leases: ${e}`));
    },

    destroy() {
      for (const u of unsubscribe) u();
      unsubscribe = [];
      if (sharedTimer) clearInterval(sharedTimer);
      sharedTimer = null;
      for (const t of restartTimers.values()) clearTimeout(t);
      restartTimers.clear();
      for (const t of consoleCloseTimers.values()) clearTimeout(t);
      consoleCloseTimers.clear();
    },
  };
}

export const stackStore = createStackStore();
