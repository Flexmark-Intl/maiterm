/** Per-window stack store (docs/stack.md).
 *
 *  A workspace declares the services its project runs; each running service IS a terminal
 *  tab whose shell stays up while the command runs in front of it (§4). This store owns
 *  the definitions' edits (persisted through `setWorkspaceStack`, a whole-list replace like
 *  tasks), and ALL of the runtime state — status, uptime, the foreground pid — which is
 *  never persisted: a parked workspace would otherwise wake up "crashed".
 *
 *  Every write into a terminal goes through the guard in §4: start only when the shell is
 *  at its prompt, signal only the pid recorded at start. A wrong "no" costs a retry; a
 *  wrong "yes" types into whatever the human left running there.
 */

import { untrack } from 'svelte';
import { error as logError, info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { Service, ServiceOrigin, ServiceRestart, Tab, Workspace } from '$lib/tauri/types';
import { workspacesStore } from './workspaces.svelte';
import { terminalsStore } from './terminals.svelte';
import { activityStore } from './activity.svelte';
import { preferencesStore } from './preferences.svelte';
import { normalizeTitle } from '$lib/tasks/model';
import {
  exitIsCrash, restartAllowed, restartDelay, rollupStatus, startLine,
  RESTART_CEILING, RESTART_WINDOW_MS, type Rollup, type ServiceStatus,
} from '$lib/stack/model';

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
}

const IDLE: ServiceRuntime = { status: 'stopped', since: null, lastExitCode: null, pid: null, ptyId: null, writeAt: null, beganAt: null, noIntegration: false, stopping: false, restarts: [], note: null };

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

function sleep(ms: number) {
  return new Promise<void>((r) => setTimeout(r, ms));
}

function createStackStore() {
  let runtime = $state<Map<string, ServiceRuntime>>(new Map());
  /** Workspaces whose auto_start already fired for this activation. Cleared on suspend. */
  const autoStarted = new Set<string>();
  const restartTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** Starts in flight, by service. The status alone cannot guard re-entry: `start` does
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
  let unsubscribe: (() => void)[] = [];

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

  function rt(serviceId: string): ServiceRuntime {
    return runtime.get(serviceId) ?? IDLE;
  }

  function setRt(serviceId: string, patch: Partial<ServiceRuntime>) {
    const next = new Map(runtime);
    next.set(serviceId, { ...rt(serviceId), ...patch });
    runtime = next;
  }

  function workspaceOf(workspaceId: string): Workspace | undefined {
    return workspacesStore.workspaces.find((w) => w.id === workspaceId);
  }

  function serviceOf(workspaceId: string, serviceId: string): Service | undefined {
    return workspaceOf(workspaceId)?.stack?.find((s) => s.id === serviceId);
  }

  /** The tab bound to a service — derived from `Tab.service_id`, never stored (§3). */
  function boundTab(workspaceId: string, serviceId: string): { pane: { id: string }; tab: Tab } | undefined {
    const ws = workspaceOf(workspaceId);
    if (!ws) return undefined;
    for (const pane of ws.panes) {
      const tab = pane.tabs.find((t) => t.service_id === serviceId);
      if (tab) return { pane, tab };
    }
    return undefined;
  }

  /** Reverse lookup for the exit hooks: which service does this tab run? */
  function serviceForTab(tabId: string): { workspaceId: string; service: Service } | undefined {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find((t) => t.id === tabId);
        if (!tab) continue;
        if (!tab.service_id) return undefined;
        const service = ws.stack?.find((s) => s.id === tab.service_id);
        return service ? { workspaceId: ws.id, service } : undefined;
      }
    }
    return undefined;
  }

  async function persist(workspaceId: string, stack: Service[]) {
    try {
      await workspacesStore.setWorkspaceStack(workspaceId, stack);
    } catch (e) {
      logError(`stack: persist failed for workspace ${workspaceId}: ${e}`);
    }
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
    const r = rt(hit.service.id);
    // The B/C for OUR line: the first one after the write, on the PTY we wrote to.
    // Anything earlier is a command the shell was already running (or the rc's own
    // hooks) and says nothing about us.
    if (r.writeAt !== null && r.beganAt === null && r.ptyId === ptyId && now >= r.writeAt && r.status === 'starting') {
      setRt(hit.service.id, { beganAt: now, status: 'running' });
    }
  }

  /** `exitCode === null` is the no-integration fallback: the tty came back with nothing
   *  to say why. With integration, a D counts only once the command's own B/C was seen. */
  function onExit(tabId: string, ptyId: string, exitCode: number | null) {
    facts(ptyId).lastExitAt = Date.now();
    const hit = serviceForTab(tabId);
    if (!hit) return;
    const { workspaceId, service } = hit;
    const r = rt(service.id);
    if (r.status === 'stopped' || r.status === 'crashed') return;
    if (r.ptyId !== null && r.ptyId !== ptyId) return; // a different shell's exit
    if (exitCode !== null && !r.noIntegration && r.beganAt === null) {
      // A D before our command began: the fresh shell's unconditional first-prompt D;0,
      // or a human's command finishing on the bound tab while we waited. Not ours.
      return;
    }
    // A deliberate stop is a stop whatever the code — `make dev` returns 2 when its child
    // is interrupted.
    const crashed = !r.stopping && (exitCode === null || exitIsCrash(exitCode));
    setRt(service.id, {
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
      logWarn(`stack: ${service.name} crashed with exit ${exitCode}`);
      scheduleRestart(workspaceId, service);
    }
  }

  function scheduleRestart(workspaceId: string, service: Service) {
    if ((service.restart ?? 'on_crash') !== 'on_crash') return;
    const r = rt(service.id);
    const now = Date.now();
    if (!restartAllowed(r.restarts, now)) {
      setRt(service.id, { note: `crashed ${RESTART_CEILING}× in 10 min — not restarting` });
      return;
    }
    const recent = r.restarts.filter((t) => now - t < RESTART_WINDOW_MS);
    const delay = restartDelay(recent.length);
    setRt(service.id, { restarts: [...recent, now], note: `restarting in ${Math.round(delay / 1000)}s` });
    const existing = restartTimers.get(service.id);
    if (existing) clearTimeout(existing);
    restartTimers.set(service.id, setTimeout(() => {
      restartTimers.delete(service.id);
      if (rt(service.id).status !== 'crashed') return;
      start(workspaceId, service.id).catch((e) => logError(`stack: auto-restart of ${service.name} failed: ${e}`));
    }, delay));
  }

  /** A bound tab that disappears (its shell died and `pty-close` deleted it, or the human
   *  closed it) takes its running service down with it — `crashed` unless a stop was in
   *  flight. No restart: there is no tab to restart in; the next start mints one. A bound
   *  tab whose live PTY is not the one the command was typed into (a reload minted a fresh
   *  shell) is `stopped`: nothing is running there, and a green dot over it would lie. */
  function reconcileBindings() {
    for (const [serviceId, r] of runtime) {
      if (r.status === 'stopped' || r.status === 'crashed') continue;
      // A start in flight owns its runtime until it returns: it is between "chose a tab"
      // and "typed into it", and any reading of its half-written fields here is wrong.
      if (startsInFlight.has(serviceId)) continue;
      // The binding only counts inside the workspace that owns the definition — a tab
      // moved elsewhere has its binding cleared by Rust, and searching every workspace
      // would keep a moved-away service "running" here with no tab to show for it.
      const ws = workspacesStore.workspaces.find((w) => w.stack?.some((s) => s.id === serviceId));
      const tab = ws?.panes.flatMap((p) => p.tabs).find((t) => t.service_id === serviceId);
      if (!tab) {
        const crashed = !r.stopping;
        setRt(serviceId, { status: crashed ? 'crashed' : 'stopped', since: null, pid: null, ptyId: null, stopping: false, note: crashed ? 'its tab closed' : null });
        if (crashed) logWarn(`stack: service ${serviceId} lost its tab`);
        continue;
      }
      // `ptyId` is recorded only once the command has been typed into a live PTY, so a
      // bound tab that now has no instance (suspendTab killed it) or a different one (a
      // reload minted a fresh shell) has nothing of ours running in it.
      const live = terminalsStore.get(tab.id);
      if (r.ptyId && (!live || live.ptyId !== r.ptyId)) {
        setRt(serviceId, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: live ? 'its tab was reloaded — start it again' : 'its tab was suspended' });
      }
    }
  }

  /** Suspending a workspace kills its PTYs: every service in it is `stopped`, and the
   *  workspace may auto-start again on its next activation. */
  function reconcileSuspended() {
    for (const ws of workspacesStore.workspaces) {
      if (!ws.suspended) continue;
      autoStarted.delete(ws.id);
      for (const s of ws.stack ?? []) {
        const r = rt(s.id);
        if (r.status !== 'stopped') setRt(s.id, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: null });
      }
    }
  }

  // ── Verbs ──────────────────────────────────────────────────────────────────────

  async function start(workspaceId: string, serviceId: string): Promise<ServiceStatus> {
    const service = serviceOf(workspaceId, serviceId);
    if (!service) throw new Error('Service not found');
    const ws = workspaceOf(workspaceId)!;
    if (ws.suspended) throw new Error('Workspace is suspended — resume it first');
    const pending = startsInFlight.get(serviceId);
    if (pending) return pending;
    const stopping = stopsInFlight.get(serviceId);
    if (stopping) await stopping.catch(() => undefined);
    const r = rt(serviceId);
    if (r.status === 'starting' || r.status === 'running' || r.status === 'ready') return r.status;
    const p = startInner(workspaceId, service).finally(() => { startsInFlight.delete(serviceId); abortStart.delete(serviceId); });
    startsInFlight.set(serviceId, p);
    return p;
  }

  /** A stop arrived while a start was still working: if the line has not been typed yet,
   *  the start aborts cleanly; if it has, the stop waits for the start to settle and then
   *  proceeds against the pid it recorded. */
  function aborted(serviceId: string): boolean {
    if (!abortStart.has(serviceId)) return false;
    setRt(serviceId, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: 'start cancelled' });
    return true;
  }

  async function startInner(workspaceId: string, service: Service): Promise<ServiceStatus> {
    const serviceId = service.id;
    const ws = workspaceOf(workspaceId)!;
    const timer = restartTimers.get(serviceId);
    if (timer) { clearTimeout(timer); restartTimers.delete(serviceId); }

    // 1. A tab to run in: the bound one, else mint one in the background.
    let bound = boundTab(workspaceId, serviceId);
    if (!bound) {
      const paneId = ws.active_pane_id ?? ws.panes[0]?.id;
      if (!paneId) throw new Error('Workspace has no pane');
      const tab = await workspacesStore.createTab(workspaceId, paneId, service.name, { append: true, background: true });
      terminalsStore.setSplitContext(tab.id, { cwd: service.cwd || null, sshCommand: null, remoteCwd: null });
      await workspacesStore.renameTab(workspaceId, paneId, tab.id, service.name, true);
      await workspacesStore.setTabServiceId(workspaceId, paneId, tab.id, serviceId);
      bound = boundTab(workspaceId, serviceId);
      if (!bound) throw new Error('Could not bind a tab');
    }
    const tabId = bound.tab.id;

    // 2. Make sure it is mounted (a background workspace's tab may not be) and live.
    const wasLive = !!terminalsStore.get(tabId);
    if (!wasLive) {
      window.dispatchEvent(new CustomEvent('activate-tab', { detail: tabId }));
      await terminalsStore.waitForRegister(tabId, MOUNT_WAIT_MS);
    }
    if (aborted(serviceId)) return 'stopped';
    const instance = terminalsStore.get(tabId);
    if (!instance) {
      setRt(serviceId, { status: 'stopped', note: 'its tab could not be mounted' });
      return 'stopped';
    }
    // A fresh run: nothing from a previous one may survive into it — a stale `ptyId` in
    // particular, which `reconcileBindings` would read as "its tab was reloaded" mid-start.
    setRt(serviceId, { status: 'starting', since: Date.now(), stopping: false, note: null, lastExitCode: null, pid: null, ptyId: null, writeAt: null, beganAt: null, noIntegration: false });

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
    const cancelled = () => abortStart.has(serviceId);
    // With integration switched off no A will ever come; don't wait 12s to learn that. (A
    // PTY reattached after a webview reload is the one case that still pays the wait: its
    // A was emitted before this store existed, and it will not prompt again until Enter.)
    const integrated = preferencesStore.shellIntegration
      ? await waitForFact(ptyId, (x) => x.lastPromptAt !== null || cancelled(), FIRST_PROMPT_WAIT_MS) && !cancelled()
      : false;
    if (aborted(serviceId)) return 'stopped';
    if (integrated) {
      const idle = await waitForFact(ptyId, (x) => promptSince(x.lastBeginAt) || cancelled(), wasLive ? 4000 : 0);
      if (aborted(serviceId)) return 'stopped';
      if (!idle) {
        const fg = await probeForeground(instance.ptyId);
        const what = fg?.executable ? `${fg.executable} is in the foreground` : 'the shell is mid-command';
        setRt(serviceId, { status: 'stopped', since: null, note: `not started — ${what}` });
        logWarn(`stack: refused to start ${service.name}: ${what}`);
        return 'stopped';
      }
    } else {
      const atPrompt = await waitForPrompt(instance.ptyId, 4000);
      if (!atPrompt) {
        const fg = await probeForeground(instance.ptyId);
        const what = fg?.executable ? `${fg.executable} is in the foreground` : 'the shell is busy';
        setRt(serviceId, { status: 'stopped', since: null, note: `not started — ${what}` });
        logWarn(`stack: refused to start ${service.name}: ${what}`);
        return 'stopped';
      }
      setRt(serviceId, { noIntegration: true });
    }
    if (aborted(serviceId)) return 'stopped';

    // 4. Type it.
    const line = startLine(service);
    setRt(serviceId, { ptyId: instance.ptyId, writeAt: Date.now() });
    await commands.writeTerminal(instance.ptyId, Array.from(new TextEncoder().encode(line + '\n')));
    logInfo(`stack: started ${service.name} in tab ${tabId}`);

    // 5. Confirm the shell took it, then record the job that holds the tty — the pid the
    //    stop guard compares against.
    if (integrated) {
      const began = await waitForFact(ptyId, () => rt(serviceId).beganAt !== null || rt(serviceId).status === 'stopped' || rt(serviceId).status === 'crashed', BEGIN_WAIT_MS);
      const now = rt(serviceId);
      if (now.status === 'stopped' || now.status === 'crashed') return now.status;
      if (!began) {
        // Thirty seconds and no B/C: the shell ate the line (an rc step reading stdin) or
        // is wedged. Disown it completely — a kept `ptyId` cannot help (`onExit` returns
        // on `stopped` first) and poisons the next start. If the line does run later it
        // is unowned; adopting "the first B/C after the fact" would as easily adopt the
        // human's `ls` when they go and look, and then SIGTERM it on Stop.
        setRt(serviceId, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, note: 'not started — the shell never ran the command; check the tab' });
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
      if (!took) { onExit(tabId, ptyId, null); return rt(serviceId).status; }
      setRt(serviceId, { status: 'running', beganAt: Date.now() });
      void watchNoIntegration(serviceId, ptyId);
    }
    for (let i = 0; i < 4; i++) {
      const fg = await probeForeground(instance.ptyId);
      if (fg?.shell_at_prompt === false && fg.pid != null) { setRt(serviceId, { pid: fg.pid }); break; }
      const now = rt(serviceId);
      if (now.status === 'stopped' || now.status === 'crashed') break;
      await sleep(300);
    }
    return rt(serviceId).status;
  }

  /** The no-integration fallback's exit watch: while a service started without OSC 133 is
   *  running, its tty returning to the shell is the only exit signal there is. */
  async function watchNoIntegration(serviceId: string, ptyId: string) {
    while (true) {
      await sleep(2000);
      const r = rt(serviceId);
      if (!r.noIntegration || r.ptyId !== ptyId || (r.status !== 'running' && r.status !== 'ready')) return;
      const fg = await probeForeground(ptyId);
      if (fg?.shell_at_prompt === true) {
        const bound = [...workspacesStore.workspaces].flatMap((w) => w.panes).flatMap((p) => p.tabs).find((t) => t.service_id === serviceId);
        if (bound) onExit(bound.id, ptyId, null);
        return;
      }
    }
  }

  async function stop(workspaceId: string, serviceId: string): Promise<ServiceStatus> {
    const service = serviceOf(workspaceId, serviceId);
    if (!service) throw new Error('Service not found');
    const pending = stopsInFlight.get(serviceId);
    if (pending) return pending;
    const starting = startsInFlight.get(serviceId);
    if (starting) {
      // Ask the start to abort if it has not typed yet, then wait for it either way.
      abortStart.add(serviceId);
      await starting.catch(() => undefined);
    }
    const r = rt(serviceId);
    if (r.status === 'stopped' || r.status === 'crashed') return r.status;
    const p = stopInner(workspaceId, serviceId, r).finally(() => stopsInFlight.delete(serviceId));
    stopsInFlight.set(serviceId, p);
    return p;
  }

  async function stopInner(workspaceId: string, serviceId: string, r: ServiceRuntime): Promise<ServiceStatus> {
    const bound = boundTab(workspaceId, serviceId);
    const instance = bound ? terminalsStore.get(bound.tab.id) : undefined;
    if (!instance || (r.ptyId && instance.ptyId !== r.ptyId)) {
      // No live shell, or a different one than the command went into: nothing is running.
      setRt(serviceId, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: null });
      return 'stopped';
    }

    // Already back at the prompt: nothing to signal, just record it.
    const fg = await probeForeground(instance.ptyId);
    if (fg?.shell_at_prompt === true) {
      setRt(serviceId, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: null });
      return 'stopped';
    }
    // The guard (§4): only the job we recorded is ever signalled. No recorded pid means we
    // cannot tell whose job holds the terminal — refuse, never fall back to "whatever is in
    // front". A wrong "yes" here ^C's and SIGTERMs the human's vim.
    if (r.pid === null || fg?.pid == null || fg.pid !== r.pid) {
      const what = fg?.executable ? `${fg.executable} is in the foreground` : 'cannot tell which job holds the terminal';
      setRt(serviceId, { note: `not stopped — ${what}` });
      return r.status;
    }
    setRt(serviceId, { stopping: true });

    const ctrlC = [0x03];
    await commands.writeTerminal(instance.ptyId, ctrlC);
    if (await waitForPrompt(instance.ptyId, 3000)) return finishStop(serviceId);
    await commands.writeTerminal(instance.ptyId, ctrlC);
    if (await waitForPrompt(instance.ptyId, 2000)) return finishStop(serviceId);

    await commands.killPtyForegroundJob(instance.ptyId, r.pid, false).catch(() => false);
    if (await waitForPrompt(instance.ptyId, 3000)) return finishStop(serviceId);
    await commands.killPtyForegroundJob(instance.ptyId, r.pid, true).catch(() => false);
    if (await waitForPrompt(instance.ptyId, 2000)) return finishStop(serviceId);

    // Gave up waiting, but the stop was asked for: leave `stopping` set so the exit, when
    // it finally comes, is filed as a stop and not a crash that auto-restarts.
    const after = await probeForeground(instance.ptyId);
    setRt(serviceId, { note: `still stopping — ${after?.executable ?? 'the job'} has not exited yet` });
    return rt(serviceId).status;
  }

  function finishStop(serviceId: string): ServiceStatus {
    // The OSC 133 exit usually lands first and clears `stopping`; this is the fallback
    // for shells without integration.
    if (rt(serviceId).status !== 'stopped') {
      setRt(serviceId, { status: 'stopped', since: null, pid: null, ptyId: null, writeAt: null, beganAt: null, stopping: false, note: null });
    }
    return 'stopped';
  }

  return {
    /** Runtime state for a service (idle defaults when never started). */
    runtime(serviceId: string): ServiceRuntime { return rt(serviceId); },
    status(serviceId: string): ServiceStatus { return rt(serviceId).status; },
    boundTab,
    serviceForTab,

    services(workspaceId: string): Service[] {
      return workspaceOf(workspaceId)?.stack ?? [];
    },

    /** Rollup for the sidebar dot: null when the stack is empty or all stopped. */
    rollup(workspaceId: string): Rollup {
      return rollupStatus(this.services(workspaceId).map((s) => ({ status: rt(s.id).status, autoStart: s.auto_start ?? true })));
    },

    // ── Definitions ─────────────────────────────────────────────────────────────

    /** Idempotent by normalized name (returns the existing service), like createTasks. */
    async createService(workspaceId: string, input: ServiceInput): Promise<Service> {
      const ws = workspaceOf(workspaceId);
      if (!ws) throw new Error('Workspace not found');
      const name = input.name.trim();
      if (!name) throw new Error('A service needs a name');
      if (!input.command.trim()) throw new Error('A service needs a command');
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
        created_at: now,
        updated_at: now,
      };
      await persist(workspaceId, [...(ws.stack ?? []), service]);
      return service;
    },

    async updateService(workspaceId: string, serviceId: string, patch: Partial<Omit<Service, 'id' | 'created_at' | 'normalized_name'>>): Promise<Service> {
      const ws = workspaceOf(workspaceId);
      const current = ws?.stack?.find((s) => s.id === serviceId);
      if (!ws || !current) throw new Error('Service not found');
      const next: Service = { ...current, ...patch, updated_at: new Date().toISOString() };
      if (patch.name !== undefined) {
        next.name = patch.name.trim();
        next.normalized_name = normalizeTitle(next.name);
      }
      await persist(workspaceId, (ws.stack ?? []).map((s) => (s.id === serviceId ? next : s)));
      return next;
    },

    /** Refused while running — stop first (the caller decides; nothing here types ^C). */
    async removeService(workspaceId: string, serviceId: string): Promise<void> {
      const ws = workspaceOf(workspaceId);
      if (!ws) throw new Error('Workspace not found');
      const r = rt(serviceId);
      if (r.status === 'starting' || r.status === 'running' || r.status === 'ready') {
        throw new Error('Stop the service before removing it');
      }
      const bound = boundTab(workspaceId, serviceId);
      if (bound) await workspacesStore.setTabServiceId(workspaceId, bound.pane.id, bound.tab.id, null);
      const next = new Map(runtime);
      next.delete(serviceId);
      runtime = next;
      await persist(workspaceId, (ws.stack ?? []).filter((s) => s.id !== serviceId));
    },

    /** An agent (or the ready trigger) reporting what it observed (§6.1, §9). */
    async reportEndpoint(workspaceId: string, serviceId: string, report: { port?: number | null; url?: string | null; ready?: boolean; note?: string | null }) {
      const patch: Partial<Service> = {};
      if (report.port !== undefined) patch.port = report.port;
      if (report.url !== undefined) patch.url = report.url;
      if (Object.keys(patch).length) await this.updateService(workspaceId, serviceId, patch);
      const r = rt(serviceId);
      if (report.ready && (r.status === 'starting' || r.status === 'running')) setRt(serviceId, { status: 'ready' });
      if (report.note !== undefined) setRt(serviceId, { note: report.note });
    },

    // ── Verbs ────────────────────────────────────────────────────────────────────

    start,
    stop,
    async restart(workspaceId: string, serviceId: string): Promise<ServiceStatus> {
      await stop(workspaceId, serviceId);
      return start(workspaceId, serviceId);
    },

    /** Every `auto_start` service, serially, in list order. */
    async startStack(workspaceId: string): Promise<void> {
      for (const s of this.services(workspaceId)) {
        if (!(s.auto_start ?? true)) continue;
        try { await start(workspaceId, s.id); } catch (e) { logError(`stack: start ${s.name}: ${e}`); }
      }
    },

    async stopStack(workspaceId: string): Promise<void> {
      for (const s of this.services(workspaceId)) {
        const st = rt(s.id).status;
        if (st === 'stopped' || st === 'crashed') continue;
        try { await stop(workspaceId, s.id); } catch (e) { logError(`stack: stop ${s.name}: ${e}`); }
      }
    },

    /** Fires once per workspace activation (§5); re-armed by suspend. */
    async autoStart(workspaceId: string): Promise<void> {
      const ws = workspaceOf(workspaceId);
      if (!ws || ws.suspended || autoStarted.has(workspaceId)) return;
      if (!(ws.stack ?? []).some((s) => s.auto_start ?? true)) return;
      autoStarted.add(workspaceId);
      await this.startStack(workspaceId);
    },

    /** Resolve when the service is ready (or running, when it has no ready pattern), else
     *  after `timeoutMs` with whatever the status is then. */
    async waitFor(workspaceId: string, serviceId: string, timeoutMs: number): Promise<ServiceStatus> {
      const service = serviceOf(workspaceId, serviceId);
      const target: ServiceStatus = service?.ready_pattern ? 'ready' : 'running';
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        const st = rt(serviceId).status;
        if (st === target || st === 'ready' || st === 'crashed' || st === 'stopped') return st;
        await sleep(250);
      }
      return rt(serviceId).status;
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
          void workspacesStore.workspaces.map((w) => [w.suspended, ...w.panes.map((p) => p.tabs.map((t) => t.service_id).join(','))]);
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
        let published = new Set<string>();
        $effect(() => {
          const rows = [...runtime.entries()].map(([service_id, r]) => ({ service_id, status: r.status, note: r.note, since_ms: r.since }));
          const ids = new Set(rows.map((r) => r.service_id));
          const removed = [...published].filter((id) => !ids.has(id));
          published = ids;
          commands.publishStackRuntime(rows, removed).catch((e) => logError(`stack: publish runtime: ${e}`));
        });
      }));
    },

    destroy() {
      for (const u of unsubscribe) u();
      unsubscribe = [];
      for (const t of restartTimers.values()) clearTimeout(t);
      restartTimers.clear();
    },
  };
}

export const stackStore = createStackStore();
