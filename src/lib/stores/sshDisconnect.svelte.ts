/**
 * SSH sessions that dropped *unexpectedly* (a network outage), as opposed to a logout: the
 * per-tab "disconnected" badge, and the unattended reconnect.
 *
 * **Rust decides what a drop is** (`src-tauri/src/pty/ssh_watch.rs`): it hears every terminal,
 * whether or not a pane is mounted for it. This store is window-level for the same reason — a
 * pane mounts only once its workspace has been opened, and on 2026-11-11 the pane-level
 * detector saw one of ~75 tabs drop. Nothing here may depend on a `TerminalPane` existing.
 *
 * The reconnect waits for the HOST, not for a timer: after a drop nothing is typed into a tab
 * until a background ssh to that host has succeeded (`probeSshHost`), so an internet outage
 * leaves the tabs at their prompts rather than with an ssh hanging in each. The probe never
 * gives up while a tab is still waiting. Whatever the human does in the tab ends the wait:
 * Rust forgets the drop at the next command start, and every reconnect re-reads that record
 * and the shell's command line immediately before it types.
 */
import { listen } from '@tauri-apps/api/event';
import { info as logInfo } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { Tab } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { enableBridge, isInteractiveSshSession } from '$lib/stores/sshMcpBridge.svelte';
import { interpolateShell } from '$lib/stores/triggers.svelte';
import { interpolateShellWith } from '$lib/triggers/shellInterpolate';
import { beginRemoteAccount, type RemoteAccountHandoff } from '$lib/utils/remoteAccountToken';
import { dispatch } from '$lib/stores/notificationDispatch';
import { isStripHidden } from '$lib/workspace/hiddenTabs';

export interface DisconnectInfo {
  /** Hostname for display (parsed from the ssh command), if known. */
  host: string | null;
  /** Cleaned ssh command (bare "user@host [flags]") to replay on reconnect. */
  sshCommand: string | null;
  /** Remote cwd to cd into on reconnect, if known. */
  remoteCwd: string | null;
  /** Remote (Claude-set) title at the moment of the drop, preserved on the tab. */
  title: string | null;
  /** Timestamp of the drop (ms). */
  at: number;
}

/** A terminal Rust has a drop on record for (`list_dropped_ssh`, the `ssh-dropped` event). */
export interface DroppedSsh {
  pty_id: string;
  tab_id: string;
  /** The raw ssh command line, as `ps` reported it. */
  ssh: string;
}

/**
 * Unattended reconnects in flight at once. An outage drops every ssh tab at the same moment —
 * hundreds of them — and each reconnect is an interactive ssh plus a bridge setup ssh to the
 * same host. Four keeps sshd's MaxStartups and the tunnel's port allocation out of it; one
 * would take minutes.
 */
const MAX_CONCURRENT_RECONNECTS = 4;

/**
 * Waits between reachability probes of one host, then the last one repeats for as long as a
 * tab is waiting. The first is ≥5s for the bridge's reason (sshMcpBridge `RECONNECT_DELAYS_MS`):
 * the far sshd still holds our old `-R` listener for a moment after the client dies.
 */
const PROBE_DELAYS_MS = [5_000, 10_000, 20_000, 40_000, 60_000];
/** Wait before looking again at tabs that could not be typed into yet (something on the line). */
const HELD_RETRY_MS = 60_000;
/** A reconnect that drops again this soon never really came up. */
const QUICK_FAIL_MS = 90_000;
/** After this many of those in a row the tab is left to its badge: a host that answers the
 *  probe and then refuses the session is not going to be fixed by typing at it again. */
const MAX_QUICK_FAILS = 5;
/** Drops on one host within this window are announced as one notification. */
const ANNOUNCE_WINDOW_MS = 3_000;

/** Best-effort hostname from an ssh command, for display and for comparing two commands. */
export function parseSshHost(sshCmd: string): string | null {
  const tokens = commands.sshArgsOf(sshCmd).split(/\s+/).filter(Boolean);
  const withUser = tokens.find(t => t.includes('@'));
  if (withUser) return withUser.split('@')[1] || null;
  const host = tokens.find(t => !t.startsWith('-'));
  return host ?? null;
}

function findTab(tabId: string): Tab | null {
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      const tab = pane.tabs.find(t => t.id === tabId);
      if (tab) return tab;
    }
  }
  return null;
}

function findTabByPty(ptyId: string): { tab: Tab; workspaceId: string } | null {
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      const tab = pane.tabs.find(t => t.pty_id === ptyId || terminalsStore.get(t.id)?.ptyId === ptyId);
      if (tab) return { tab, workspaceId: ws.id };
    }
  }
  return null;
}

function ptyOf(tab: Tab): string | null {
  return terminalsStore.get(tab.id)?.ptyId ?? tab.pty_id ?? null;
}

/** Does this tab resume its agent on `host`? Only then does the saved resume command (and the
 *  saved remote folder) belong to a reconnect there. */
function resumesOn(tab: Tab, host: string | null): boolean {
  return tab.auto_resume_enabled !== false && !!tab.auto_resume_ssh_command
    && parseSshHost(tab.auto_resume_ssh_command) === host;
}

/**
 * What a reconnect needs, from the tab's SAVED state. Deliberately not the live OSC state: by
 * the time a drop is known the local shell has drawn its prompt, and a "current folder" read
 * then is a folder on this computer — `cd` to it on the remote fails and ends the new session.
 */
export function dropInfo(tab: Tab, sshCommand: string, title: string | null): DisconnectInfo {
  const host = parseSshHost(sshCommand);
  let remoteCwd: string | null = null;
  if (resumesOn(tab, host)) remoteCwd = tab.auto_resume_remote_cwd ?? null;
  else if (tab.restore_ssh_command && parseSshHost(tab.restore_ssh_command) === host) remoteCwd = tab.restore_remote_cwd ?? null;
  return { host, sshCommand, remoteCwd, title: title ?? tab.name ?? null, at: Date.now() };
}

/**
 * Type the ssh command back into a tab's still-alive local shell, then — once the connection
 * is up — bring back its MCP bridge and the agent. Needs no mounted pane.
 *
 * `unattended` (no human watching this tab) gates the resume on a PROVEN connection: "an ssh
 * process is in the foreground" is not "ssh is connected" — an ssh to an unreachable host sits
 * in TCP connect for ~75s, and a resume command typed then waits in the tty queue until ssh
 * exits 255 and the LOCAL shell runs it. So there the resume is typed only once the bridge is up
 * (its setup ssh reached the same host and ran a script there) and the interactive ssh is still
 * the foreground job.
 *
 * Resolves to whether the ssh command was typed.
 */
export async function replaySsh(tabId: string, o: {
  unattended: boolean;
  /** Type the tab's saved resume command once connected. */
  resume: boolean;
  /**
   * May maiTerm type into this tab: the keys to send ahead of the command, or `null` to type
   * nothing. Asked twice. First before anything is built, because building pushes the tab's
   * account token to the host and a tab that is going to be refused must not cost a credential
   * file there every minute it waits. Then again as the last word, with nothing left to await
   * but the write: everything slow is behind it, so what it reads about the shell is still
   * true when the write lands.
   */
  gate?: (final: boolean) => Promise<string | null>;
  isGone?: () => boolean;
  /** The command is in the terminal: the caller's own bookkeeping for "this ssh is ours". */
  onTyped?: (sshCommand: string) => void;
}): Promise<boolean> {
  // One at a time per tab. The badge stays up while the command is being built (seconds, with
  // an account handoff), and a second click — or a click during the store's own reconnect —
  // would type a second ssh into the first one as it connects, to be run on the remote host.
  if (replaying.has(tabId)) return false;
  replaying.add(tabId);
  try {
    return await replaySshOnce(tabId, o);
  } finally {
    replaying.delete(tabId);
  }
}

const replaying = new Set<string>();

async function replaySshOnce(tabId: string, o: Parameters<typeof replaySsh>[1]): Promise<boolean> {
  const gone = () => o.isGone?.() ?? false;
  const tab = findTab(tabId);
  const ptyId = tab ? ptyOf(tab) : null;
  if (!tab || !ptyId) return false;
  const info = sshDisconnectStore.getInfo(tabId);
  const sshCommand = info?.sshCommand ?? tab.auto_resume_ssh_command ?? tab.restore_ssh_command ?? null;
  if (!sshCommand) { logInfo(`SSH reconnect: no ssh command for tab ${tabId}`); return false; }
  const remoteCwd = info?.remoteCwd ?? tab.auto_resume_remote_cwd ?? tab.restore_remote_cwd ?? null;

  if (o.gate && (await o.gate(false)) === null) return false;

  let bakedPort: number | undefined;
  let handoff: RemoteAccountHandoff | undefined;
  try {
    const bridgeEnv = await commands.getRemoteBridgeEnv(sshCommand);
    bakedPort = bridgeEnv?.port;
    // Re-asked on every reconnect rather than remembered from the first connect: the handoff
    // file is consumed by the shell that read it, and the active account may have changed
    // while this tab was down.
    handoff = beginRemoteAccount(tabId, sshCommand);
    const accountExport = await handoff.take();
    const cmd = commands.buildSshCommand(sshCommand, remoteCwd, tabId, bridgeEnv, accountExport);
    const prefix = gone() ? null : o.gate ? await o.gate(true) : '';
    if (prefix === null || gone()) {
      // Built, not typed: the token file on the host would never be read, or removed.
      await handoff.retract();
      return false;
    }
    // Rust is told which ssh this is BEFORE it exists: its own probe looks 1.2s after the
    // command starts, and a reconnect refused sooner than that (sshd shedding a burst of new
    // connections) would otherwise exit 255 with nothing on record to credit it to — the tab
    // left at its prompt with no badge and no retry.
    await commands.noteTypedSsh(ptyId, cmd).catch(() => {});
    // Stop preserving the (now stale) title and drop the badge — a fresh remote title arrives
    // once the agent restarts. If this ssh dies too, Rust records it and the badge comes back.
    sshDisconnectStore.clear(tabId);
    await commands.writeTerminal(ptyId, Array.from(new TextEncoder().encode(`${prefix}${cmd}\n`)));
  } catch (e) {
    logInfo(`SSH reconnect: failed to type the ssh command into tab ${tabId}: ${e}`);
    await handoff?.retract();
    return false;
  }
  o.onTyped?.(sshCommand);

  // Wait for the ssh process to hold the terminal (15s).
  for (let i = 0; ; i++) {
    if (gone()) return true;
    await new Promise(r => setTimeout(r, 500));
    try {
      if ((await commands.getPtyInfo(ptyId)).foreground_command) break;
    } catch { return true; } // tab gone
    if (i >= 29) return true;
  }
  if (gone()) return true;
  void commands.bindRemoteAccount(tabId).catch(() => {}); // maiLink §14
  const bridged = await enableBridge(tabId, sshCommand, ptyId, false, bakedPort).catch(() => false);
  if (gone()) return true;
  if (o.unattended) {
    const fg = await commands.getPtyForegroundJob(ptyId).catch(() => null);
    if (!bridged || fg?.shell_at_prompt !== false) {
      logInfo(`SSH reconnect: tab ${tabId} not resuming — ${bridged ? 'ssh is no longer the foreground job' : `no bridge to ${parseSshHost(sshCommand) ?? '?'}, so no proof the host answered`}`);
      return true;
    }
  }
  const resumeCmd = o.resume ? (tab.auto_resume_command ?? tab.auto_resume_remembered_command ?? null) : null;
  if (!resumeCmd) return true;
  // A mounted pane has the tab's live variables; an unmounted one has only what was saved.
  const text = terminalsStore.get(tabId)
    ? interpolateShell(tabId, resumeCmd, true)
    : interpolateShellWith(new Map(Object.entries(tab.trigger_variables ?? {})), resumeCmd, true);
  try {
    await commands.writeTerminal(ptyId, Array.from(new TextEncoder().encode(text + '\n')));
  } catch (e) {
    logInfo(`SSH reconnect: failed to send the resume command to tab ${tabId}: ${e}`);
  }
  return true;
}

interface WaitingHost {
  tabs: Set<string>;
  attempt: number;
  timer?: ReturnType<typeof setTimeout>;
  running: boolean;
}

function createSshDisconnectStore() {
  let disconnected = $state<Map<string, DisconnectInfo>>(new Map());
  let running = 0;
  const waiting: (() => void)[] = [];

  /** Tabs waiting for their host to answer, keyed by the ssh arguments that reach it. */
  const hosts = new Map<string, WaitingHost>();
  /** tabId → when maiTerm last typed a reconnect, and how many in a row dropped straight away. */
  const typed = new Map<string, { at: number; fails: number }>();
  /** host → the drops not yet announced. */
  const announcing = new Map<string, { tabIds: string[]; timer: ReturnType<typeof setTimeout> }>();
  let started = false;

  async function throttle<T>(fn: () => Promise<T>): Promise<T> {
    // A finishing reconnect hands its slot straight to the next in line, so a newcomer
    // cannot slip in between and make a fifth.
    if (running >= MAX_CONCURRENT_RECONNECTS) await new Promise<void>(resolve => waiting.push(resolve));
    else running++;
    try {
      return await fn();
    } finally {
      const next = waiting.shift();
      if (next) next();
      else running--;
    }
  }

  function mark(tabId: string, info: DisconnectInfo) {
    disconnected = new Map(disconnected);
    disconnected.set(tabId, info);
  }

  function clear(tabId: string) {
    if (!disconnected.has(tabId)) return;
    disconnected = new Map(disconnected);
    disconnected.delete(tabId);
  }

  /** One notification per host per burst, not one per tab: an outage takes dozens at once. */
  function announce(tabId: string, host: string | null) {
    const key = host ?? '';
    const batch = announcing.get(key);
    if (batch) { batch.tabIds.push(tabId); return; }
    const entry = {
      tabIds: [tabId],
      timer: setTimeout(() => {
        announcing.delete(key);
        const n = entry.tabIds.length;
        const where = host ? ` to ${host}` : '';
        dispatch(
          'SSH disconnected',
          n === 1
            ? `The connection${where} dropped. maiTerm reconnects the tab when the host answers.`
            : `The connection${where} dropped in ${n} tabs. maiTerm reconnects them when the host answers.`,
          'error',
          { tabId: entry.tabIds[0] },
        );
      }, ANNOUNCE_WINDOW_MS),
    };
    announcing.set(key, entry);
  }

  function schedule(key: string, delayMs?: number) {
    const host = hosts.get(key);
    if (!host) return;
    clearTimeout(host.timer);
    const base = delayMs ?? PROBE_DELAYS_MS[Math.min(host.attempt, PROBE_DELAYS_MS.length - 1)];
    // Jittered so hosts that died together do not all re-authenticate in the same second.
    host.timer = setTimeout(() => {
      host.timer = undefined;
      void pass(key);
    }, Math.round(base * (0.8 + Math.random() * 0.4)));
  }

  /** Put a dropped tab in line for its host. Safe to call again for a tab already waiting. */
  function queue(tabId: string) {
    const info = disconnected.get(tabId);
    if (!info?.sshCommand) return;
    const key = commands.sshArgsOf(info.sshCommand);
    let host = hosts.get(key);
    if (!host) {
      host = { tabs: new Set(), attempt: 0, running: false };
      hosts.set(key, host);
    }
    host.tabs.add(tabId);
    if (!host.timer && !host.running) schedule(key);
  }

  /**
   * One tab's reconnect, with the host known to answer. `typed`: the ssh is in the terminal.
   * `held`: not now, look again later. `left`: this tab is no longer maiTerm's to reconnect.
   */
  async function reconnectOne(tabId: string): Promise<'typed' | 'held' | 'left'> {
    const tab = findTab(tabId);
    const info = disconnected.get(tabId);
    const ptyId = tab ? ptyOf(tab) : null;
    if (!tab || !info || !ptyId || terminalsStore.shuttingDown) return 'left';
    if (workspacesStore.isTabSuspending(tabId)) return 'left';
    if ((typed.get(tabId)?.fails ?? 0) >= MAX_QUICK_FAILS) {
      logInfo(`SSH reconnect: leaving tab ${tabId} to its badge — ${MAX_QUICK_FAILS} reconnects to ${info.host ?? '?'} in a row dropped straight away`);
      return 'left';
    }

    let why: 'held' | 'left' = 'held';
    /**
     * Asked by `replaySsh` before it builds the command, and again (`final`) with only the
     * write left. This tab may have waited a minute behind others on its host, so nothing
     * read before now counts.
     */
    const gate = async (final: boolean): Promise<string | null> => {
      // Rust forgets a drop at the next command start, so a tab missing here is one the human
      // has used since (their own ssh, or anything else). Also true of a drop only a pane's
      // fallback saw (no shell integration): with nothing to say the line is empty, that one
      // stays a badge to click.
      const onRecord = await commands.listDroppedSsh().catch(() => null);
      if (!onRecord) return null;
      if (!onRecord.some(d => d.pty_id === ptyId)) { why = 'left'; return null; }
      const fg = await commands.getPtyForegroundJob(ptyId).catch(() => null);
      if (fg?.shell_at_prompt !== true) return null;
      // LAST, nearest the write: is the shell's command line empty? Typed onto a human's
      // half-written command, the ssh would be glued to it and run.
      const line = await commands.probeShellLine(ptyId).catch(() => null);
      if (!line) {
        logInfo(`SSH reconnect: leaving tab ${tabId} to its badge — its shell can't say whether its command line is empty`);
        why = 'left';
        return null;
      }
      if (line.len > 0) return null;
      if (final) typed.set(tabId, { at: Date.now(), fails: typed.get(tabId)?.fails ?? 0 });
      // In bash the line goes behind Ctrl-G Ctrl-U, which cancels a prefix key typed ahead
      // that the grid cannot show (docs/follow-ups.md §6.2).
      return line.shell === 'bash' ? '\x07\x15' : '';
    };

    const resume = resumesOn(tab, info.host);
    // A mounted pane does the typing itself, so its own watchers know this ssh is maiTerm's.
    const detail: { tabId: string; unattended: boolean; resume: boolean; gate: typeof gate; taken?: Promise<boolean> } =
      { tabId, unattended: true, resume, gate };
    window.dispatchEvent(new CustomEvent('ssh-reconnect', { detail }));
    const didType = await (detail.taken ?? replaySsh(tabId, { unattended: true, resume, gate }));
    return didType ? 'typed' : why;
  }

  async function pass(key: string) {
    const host = hosts.get(key);
    if (!host || host.running) return;
    host.running = true;
    let again: number | undefined;
    try {
      for (const tabId of [...host.tabs]) {
        if (!disconnected.has(tabId) || !findTab(tabId)) host.tabs.delete(tabId);
      }
      if (host.tabs.size === 0) return;

      if (!(await commands.probeSshHost(key).catch(() => false))) {
        if (host.attempt === 0) logInfo(`SSH reconnect: ${key} does not answer — ${host.tabs.size} tab(s) wait for it`);
        host.attempt += 1;
        again = PROBE_DELAYS_MS[Math.min(host.attempt, PROBE_DELAYS_MS.length - 1)];
        return;
      }
      host.attempt = 0;
      const batch = [...host.tabs];
      logInfo(`SSH reconnect: ${key} answers — reconnecting ${batch.length} tab(s)`);
      const outcomes = await Promise.all(batch.map(tabId =>
        throttle(() => reconnectOne(tabId).catch(() => 'held' as const)).then(outcome => ({ tabId, outcome })),
      ));
      let done = 0;
      for (const { tabId, outcome } of outcomes) {
        if (outcome === 'typed') done += 1;
        // A reconnect that has ALREADY dropped again (its re-drop was queued while this pass
        // ran, and found the tab still in the set) keeps its place: removing it here would
        // leave a badged tab with no timer and nobody coming back for it.
        if (outcome === 'left' || (outcome === 'typed' && !disconnected.has(tabId))) host.tabs.delete(tabId);
      }
      logInfo(`SSH reconnect: ${key} — typed into ${done} of ${batch.length} tab(s)${host.tabs.size ? `, ${host.tabs.size} still waiting` : ''}`);
      // Tabs that dropped while this pass ran, or that had something on their command line.
      if (host.tabs.size > 0) again = outcomes.some(o => o.outcome === 'held') ? HELD_RETRY_MS : PROBE_DELAYS_MS[0];
    } finally {
      host.running = false;
      if (host.tabs.size === 0) { clearTimeout(host.timer); hosts.delete(key); }
      else schedule(key, again);
    }
  }

  /**
   * A drop, from wherever it was seen. Idempotent: a tab already badged is only made sure of
   * its place in line — the same drop is reported by Rust and, when a pane is mounted, by it.
   */
  function noteDrop(tabId: string, info: DisconnectInfo, reason: string) {
    if (terminalsStore.shuttingDown) return;
    if (!disconnected.has(tabId)) {
      const last = typed.get(tabId);
      const quick = !!last && Date.now() - last.at < QUICK_FAIL_MS;
      if (last) typed.set(tabId, { at: last.at, fails: quick ? last.fails + 1 : 0 });
      mark(tabId, info);
      logInfo(`SSH drop detected for tab ${tabId} (${reason}) host=${info.host ?? '?'}`);
      // A reconnect of ours dropping again is the same outage, already announced.
      if (!quick) announce(tabId, info.host);
    }
    queue(tabId);
  }

  /** A drop Rust recorded. Ignores terminals that are another window's. */
  function intake(row: DroppedSsh) {
    const found = findTabByPty(row.pty_id) ?? (() => {
      const tab = row.tab_id ? findTab(row.tab_id) : null;
      const ws = tab ? workspacesStore.workspaces.find(w => w.panes.some(p => p.tabs.includes(tab))) : null;
      return tab && ws ? { tab, workspaceId: ws.id } : null;
    })();
    if (!found) return;
    const { tab, workspaceId } = found;
    // Not a session to come back to: a one-shot `ssh host cmd`, a git or batch connection.
    if (row.ssh.includes('git@') || row.ssh.includes('BatchMode=yes')) return;
    const sshCommand = commands.cleanSshCommand(row.ssh);
    if (!isInteractiveSshSession(sshCommand)) return;
    // Intentional teardown is never a drop, and a tab outside the strip is not the human's ssh.
    if (workspacesStore.isTabSuspending(tab.id) || workspacesStore.isWorkspaceSuspending(workspaceId)) return;
    if (isStripHidden(tab)) return;
    // A mounted pane reports it instead: it knows the remote title the local prompt has just
    // overwritten, and puts it back.
    const detail = { tabId: tab.id, sshCommand, claimed: false };
    window.dispatchEvent(new CustomEvent('ssh-drop-detected', { detail }));
    if (!detail.claimed) noteDrop(tab.id, dropInfo(tab, sshCommand, null), 'exit-255');
  }

  /** Read every drop Rust has on record: what this webview did not hear, or was rebuilt after. */
  async function sync() {
    for (const row of await commands.listDroppedSsh().catch(() => [])) intake(row);
  }

  return {
    /** Reactive accessor — reading this in a template/`$derived` tracks changes. */
    get map() { return disconnected; },

    isDisconnected(tabId: string): boolean {
      return disconnected.has(tabId);
    },

    getInfo(tabId: string): DisconnectInfo | undefined {
      return disconnected.get(tabId);
    },

    mark,
    clear,
    noteDrop,

    /** A badge handed over with a tab moved in from another window: take its place in line here. */
    adopt(tabId: string, info: DisconnectInfo) {
      mark(tabId, info);
      queue(tabId);
    },

    /** Once per window, after the workspaces are loaded. */
    init() {
      if (started) return;
      started = true;
      void listen<DroppedSsh>('ssh-dropped', (e) => intake(e.payload));
      // The network coming back is the moment every waiting host is worth asking again.
      window.addEventListener('online', () => this.retryNow('network back'));
      void sync();
    },

    /**
     * Ask every waiting host again now instead of on its backoff, and pick up drops this
     * webview missed. Called when the displays come back: a webview behind dark displays is
     * throttled, so Rust may have recorded drops it has not been told about yet.
     */
    retryNow(reason: string) {
      void sync().then(() => {
        let i = 0;
        for (const [key, host] of hosts) {
          if (host.running) continue;
          host.attempt = 0;
          schedule(key, 1_000 + i * 1_500);
          i += 1;
        }
        if (i > 0) logInfo(`SSH reconnect: ${reason} — asking ${i} waiting host(s) now`);
      });
    },
  };
}

export const sshDisconnectStore = createSshDisconnectStore();
