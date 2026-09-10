/**
 * SSH MCP Bridge — manages reverse SSH tunnels to expose local MCP tools
 * to Claude Code instances running on remote servers.
 *
 * Flow:
 * 1. TerminalPane detects SSH session (via getPtyInfo foreground_command)
 * 2. enableBridge() called → spawns reverse tunnel → writes lockfile via background SSH
 * 3. Claude Code on remote discovers lockfile → connects through tunnel → local MCP
 * 4. On tab close → disableBridge() → decrements ref count → kills tunnel if last
 */

import * as commands from '$lib/tauri/commands';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { dispatch } from '$lib/stores/notificationDispatch';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import { setVariable } from '$lib/stores/triggers.svelte';
import { agentStateStore } from '$lib/stores/agentState.svelte';
import { countedListen as listen } from '$lib/utils/listenCounter';
import type { UnlistenFn } from '@tauri-apps/api/event';

/**
 * 'reconnecting' is a tunnel that died underneath a bridged tab and is being rebuilt
 * automatically (see queueReconnect). It short-circuits nothing: enableBridge treats it
 * like 'failed' and proceeds, so the scheduler and the term-title loop can both act on it.
 */
export type BridgeStatus = 'connected' | 'pending' | 'reconnecting' | 'failed';

interface BridgeState {
  hostKey: string;
  remotePort: number;
  status: BridgeStatus;
  error?: string;
  /** The PTY the tab owned when bridged — what a later rebuild probes before spending a connection. */
  ptyId?: string;
}

/** Reactive map of tabId → bridge state. Svelte 5 $state for reactivity in TerminalTabs. */
let bridgeStates = $state<Map<string, BridgeState>>(new Map());

/** Per-tab event listeners for tunnel-down events from Rust. */
const tunnelListeners = new Map<string, UnlistenFn>();

/**
 * Remote ports for which we've already injected `export MAITERM_TAB_ID/PORT` into
 * the live shell, keyed by tabId. The env vars persist for the whole ssh session,
 * so injecting once is enough. Without this, a bridge whose remote setup keeps
 * timing out stays in 'failed' state and the term-title retry loop re-injects the
 * export line into the user's interactive shell on every prompt — visible spam.
 * Keyed by port so a genuine reconnect (new tunnel port) re-injects.
 */
const injectedEnvPort = new Map<string, number>();

/**
 * Tabs whose tunnel died, grouped by the host they were bridged to, waiting on an
 * automatic rebuild. A tunnel-down used to just clear the tab's state and stop; the
 * only thing that could bring the bridge back was the term-title retry loop, which is
 * driven by terminal output — and an IDLE agent tab produces none. So the tabs most
 * likely to be bridged were exactly the ones that could never recover on their own:
 * on 2026-09-08 a display-sleep reap took 27 tabs down across four hosts, 3 came back
 * (because their tabs happened to be reloaded), and nothing was attempted for the rest.
 */
interface DownHost {
  /** tabId → the PTY it owned when the tunnel dropped; gates the rebuild (see prune). */
  tabs: Map<string, string | undefined>;
  attempt: number;
  timer?: ReturnType<typeof setTimeout>;
  running: boolean;
}
const downHosts = new Map<string, DownHost>();

/**
 * Backoff schedule, ~4.6 minutes end to end. The first wait is deliberately ≥5s rather
 * than immediate: the far sshd still holds our old `-R` listener until it notices the
 * client is gone, and asking for that port back too soon fails "taken" and walks us onto
 * a different one — which a RUNNING agent, with MAITERM_PORT already fixed in its
 * environment, can never follow (its MCP would stay dead for the session's whole life).
 * A short pause is what lets the same port come back. Jittered per host so hosts that
 * died together do not re-authenticate in lockstep.
 */
const RECONNECT_DELAYS_MS = [5_000, 10_000, 20_000, 40_000, 80_000, 120_000];
/** Gap between tabs on one host during a pass — keeps N setup connections out of one second. */
const RECONNECT_TAB_STAGGER_MS = 300;

/**
 * Start listening for tunnel-down events from Rust for this tab.
 */
async function listenForTunnelDown(tabId: string): Promise<void> {
  // Already listening?
  if (tunnelListeners.has(tabId)) return;
  const unlisten = await listen(`ssh-tunnel-down-${tabId}`, () => {
    logInfo(`Received ssh-tunnel-down for tab ${tabId}`);
    // Drop the listener now; a successful rebuild registers a fresh one.
    cleanupListener(tabId);
    queueReconnect(tabId);
  });
  tunnelListeners.set(tabId, unlisten);
}

/** Park a tab whose tunnel died and make sure its host has a rebuild scheduled. */
function queueReconnect(tabId: string): void {
  const st = bridgeStates.get(tabId);
  if (!st) return;
  // The port may change on rebuild; a stale "already injected" record would then skip
  // the correction on a tab that needs it.
  injectedEnvPort.delete(tabId);
  bridgeStates = new Map(bridgeStates.set(tabId, { ...st, remotePort: 0, status: 'reconnecting', error: undefined }));
  let host = downHosts.get(st.hostKey);
  if (!host) {
    host = { tabs: new Map(), attempt: 0, running: false };
    downHosts.set(st.hostKey, host);
  }
  host.tabs.set(tabId, st.ptyId);
  // An exhausted entry (see the give-up branch) getting a fresh tunnel-down means some
  // other path brought a tunnel back up and it died again — the host is reachable, so
  // it earns a fresh budget rather than one attempt and an immediate give-up.
  if (host.attempt >= RECONNECT_DELAYS_MS.length) host.attempt = 0;
  // One timer per host: the tunnel-down events for its tabs arrive within milliseconds.
  if (!host.timer && !host.running) scheduleHostReconnect(st.hostKey);
}

function scheduleHostReconnect(hostKey: string, overrideDelayMs?: number): void {
  const host = downHosts.get(hostKey);
  if (!host) return;
  clearTimeout(host.timer);
  const base = overrideDelayMs ?? RECONNECT_DELAYS_MS[Math.min(host.attempt, RECONNECT_DELAYS_MS.length - 1)];
  const jittered = Math.round(base * (0.8 + Math.random() * 0.4));
  host.timer = setTimeout(() => {
    host.timer = undefined;
    void attemptHostReconnect(hostKey);
  }, jittered);
}

/**
 * A torn-down tab (logout, close, hop to another host) must leave every host's retry
 * set at once. Left in, it would be rebuilt against the host it was on when its tunnel
 * died — the title loop re-creates its state under the NEW host within seconds, and a
 * rebuild keyed on the old one then holds a tunnel to host1 for a shell sitting on
 * host2, writes host1's ~/.aiterm with this tab's identity, and shows a green bolt over
 * an agent that has no MCP at all.
 */
function forgetDownTab(tabId: string): void {
  for (const [hostKey, host] of downHosts) {
    if (!host.tabs.delete(tabId)) continue;
    if (host.tabs.size === 0 && !host.running) {
      clearTimeout(host.timer);
      downHosts.delete(hostKey);
    }
  }
}

async function attemptHostReconnect(hostKey: string): Promise<void> {
  const host = downHosts.get(hostKey);
  if (!host || host.running) return;
  host.running = true;
  try {
    // Prune before spending a connection: a tab healed by another path (the title loop
    // got there first), torn down meanwhile (logout / close — disableBridge removed its
    // state), or whose interactive ssh went with the tunnel. That last one matters most:
    // bridging follows the shell, so with no remote shell there is nothing to bridge, and
    // when the user reconnects, the title-driven path bridges the new session fresh.
    for (const [tabId, ptyId] of [...host.tabs]) {
      const st = bridgeStates.get(tabId);
      // A tab whose state now names a different host has moved on; whatever bridged it
      // there owns it. Rebuilding it here would re-point it at the host it left.
      if (!st || st.hostKey !== hostKey || st.status === 'connected') { host.tabs.delete(tabId); continue; }
      if (ptyId && !(await isRemoteShellForeground(ptyId))) {
        bridgeStates.delete(tabId);
        bridgeStates = new Map(bridgeStates);
        host.tabs.delete(tabId);
        logInfo(`SSH MCP bridge: not reconnecting tab ${tabId} — its ssh session is gone`);
      }
    }
    if (host.tabs.size === 0) { downHosts.delete(hostKey); return; }

    host.attempt += 1;
    logInfo(`SSH MCP bridge: reconnect attempt ${host.attempt}/${RECONNECT_DELAYS_MS.length} to ${hostKey} for ${host.tabs.size} tab(s)`);
    // Sequential, not parallel: the first tab re-establishes the tunnel (one ssh auth) and
    // the rest join it through Rust's alive-pid fast path. Each still runs its own remote
    // setup connection, so stagger them — sshd's MaxStartups counts unauthenticated
    // connections per burst, and tripping it is how eca dropped us on Aug 25.
    let first = true;
    for (const [tabId, ptyId] of host.tabs) {
      if (!first) await new Promise(r => setTimeout(r, RECONNECT_TAB_STAGGER_MS));
      first = false;
      // Re-check right before spending the connection: an earlier tab's await is a window
      // in which this one can be torn down or re-bridged elsewhere.
      if (bridgeStates.get(tabId)?.hostKey !== hostKey) continue;
      // freshSsh=false: we did not watch this shell connect, so nothing is typed into it —
      // the remote already carries its env, and on an agent tab a write would land in chat.
      try { await enableBridge(tabId, hostKey, ptyId); } catch { /* recorded as 'failed' */ }
    }

    // Count successes explicitly. Emptiness is NOT success: as each tab's ssh dies its
    // prompt-return calls disableBridge, which pulls it out of this set — so a host whose
    // every tab was torn down looks identical to one that fully recovered. On 2026-09-08
    // that logged "reconnected root@tokenserver" one line after "failed … Timeout", with
    // zero tabs actually re-bridged.
    let connected = 0;
    for (const tabId of [...host.tabs.keys()]) {
      const s = bridgeStates.get(tabId);
      if (!s || s.hostKey !== hostKey) { host.tabs.delete(tabId); continue; }
      if (s.status === 'connected') { host.tabs.delete(tabId); connected += 1; }
    }
    if (host.tabs.size === 0) {
      logInfo(connected > 0
        ? `SSH MCP bridge: reconnected ${hostKey} (${connected} tab(s))`
        : `SSH MCP bridge: nothing left to reconnect on ${hostKey} — its tabs were torn down`);
      downHosts.delete(hostKey);
      return;
    }
    if (host.attempt < RECONNECT_DELAYS_MS.length) {
      // enableBridgeInner leaves a lost attempt as 'failed'; while we still mean to retry,
      // show it as what it is rather than flashing red on every pass.
      for (const tabId of host.tabs.keys()) {
        const s = bridgeStates.get(tabId);
        if (s && s.hostKey === hostKey) bridgeStates.set(tabId, { ...s, status: 'reconnecting' });
      }
      bridgeStates = new Map(bridgeStates);
      scheduleHostReconnect(hostKey);
      return;
    }
    // Out of attempts. Leave the tabs 'failed' — the honest state — and say so ONCE per
    // host: enableBridgeInner suppresses its per-attempt toast for retries precisely so
    // that this is the only notification a host that will not come back produces.
    //
    // The entry itself STAYS, exhausted (no timer, attempt at the cap). It is the only
    // registry the display-wake edge reads: deleting it here would mean six attempts
    // burned during a long sleep leave the tabs exactly where they were before this
    // scheduler existed, with nothing for the wake to pull forward. A wake resets the
    // budget; so does a fresh tunnel-down (see queueReconnect).
    const [firstTab] = host.tabs.keys();
    dispatch('MCP bridge down',
      `Could not reconnect to ${hostKey} after ${host.attempt} attempts — ${host.tabs.size} tab(s) affected`,
      'error', { tabId: firstTab });
  } finally {
    host.running = false;
  }
}

/**
 * Pull every host still waiting on a backoff timer forward to now. Called on the
 * display-wake edge: a reap that landed while the displays were dark was almost
 * certainly this machine stalling under App Nap, not the peer dying, so once we are
 * back there is nothing left to wait out. Hosts are staggered a little so that four
 * re-authentications do not land in the same second.
 */
export function retryDownBridgesNow(reason: string): void {
  let i = 0;
  for (const [hostKey, host] of downHosts) {
    if (host.running) continue;
    host.attempt = 0;
    scheduleHostReconnect(hostKey, 1_000 + i * 1_500);
    i += 1;
  }
  if (i > 0) logInfo(`SSH MCP bridge: ${reason} — retrying ${i} down host(s) now`);
}

function cleanupListener(tabId: string): void {
  const unlisten = tunnelListeners.get(tabId);
  if (unlisten) {
    unlisten();
    tunnelListeners.delete(tabId);
  }
}

/**
 * Extract host_key (user@host with non-standard flags) from a cleaned SSH command.
 * Input is already cleaned by cleanSshCommand() — e.g. "user@host" or "-p 2222 user@host"
 */
function extractHostKey(sshArgs: string): string {
  return sshArgs.trim();
}

/**
 * Whether this maiTerm has another (non-failed) bridged tab on the same host.
 * All tabs on one host share ONE reverse tunnel/port, so an env-less agent can't be
 * disambiguated — the shared `~/.aiterm` fallback would hand it a sibling tab's
 * identity (see buildSetupScript). Gates that file: write it only on a sole-tab host.
 * `excludeTabId` skips self (already in bridgeStates as 'pending' at the call site).
 */
function isSharedHost(hostKey: string, excludeTabId: string): boolean {
  for (const [id, state] of bridgeStates) {
    if (id === excludeTabId) continue;
    if (state.hostKey === hostKey && state.status !== 'failed') return true;
  }
  return false;
}

/**
 * SSH short flags that take a following argument.
 * See `man ssh` OPTIONS section.
 */
const SSH_FLAGS_WITH_ARG = new Set([
  '-b', '-c', '-D', '-E', '-e', '-F', '-I', '-i', '-J', '-L',
  '-l', '-m', '-O', '-o', '-p', '-Q', '-R', '-S', '-W', '-w',
]);

/**
 * Detect whether an ssh process is running an interactive shell or a one-shot remote command.
 *
 * One-shot commands (e.g. `ssh host 'some-cmd'` from Claude Code's Bash tool) must NOT
 * trigger the MCP bridge: by the time the tunnel is set up (~1-2s), the one-shot ssh has
 * already exited, so the env-var injection lands in the LOCAL shell instead of the remote.
 *
 * Returns true when:
 *   - ssh has no trailing remote command (pure interactive), OR
 *   - trailing command contains `exec $SHELL` (maiTerm's split/restore reconnect pattern)
 */
export function isInteractiveSshSession(cmd: string): boolean {
  const tokens = cmd.replace(/^ssh\s+/, '').split(/\s+/).filter(Boolean);
  let i = 0;
  let sawHost = false;
  while (i < tokens.length) {
    const t = tokens[i];
    if (t.startsWith('-')) {
      // Known flag+arg pair, unless written as -oKey=Value (combined).
      if (SSH_FLAGS_WITH_ARG.has(t) && t.length === 2) i += 2;
      else i += 1;
    } else if (!sawHost) {
      sawHost = true;
      i += 1;
    } else {
      // Trailing remote command — interactive only if it keeps a login shell alive.
      const remote = tokens.slice(i).join(' ');
      return /\bexec\s+\$?SHELL\b/.test(remote);
    }
  }
  return true;
}

/**
 * True when the tab's PTY currently has an interactive ssh session in the
 * foreground — the precondition for writing bridge setup / env-var commands
 * to the PTY. Writing when ssh has exited dumps the script into the LOCAL
 * shell, which clobbers local ~/.claude.json / ~/.claude/settings.json /
 * ~/.aiterm with remote-tunnel ports that are dead on this machine.
 */
export async function isRemoteShellForeground(ptyId: string): Promise<boolean> {
  try {
    // Foreground-only probe (no lsof cwd) — this runs right before the env-var
    // injection, which races the user's first keystrokes at the remote prompt.
    // ALWAYS fresh: this is the guard that keeps the export out of the LOCAL
    // shell, and the failure it prevents is caused by a *stale positive* — a
    // cached snapshot still showing the ssh that just exited. Reading that from
    // the 800ms poll cache is precisely how a remote-port export lands locally.
    const cmd = await commands.getPtyForeground(ptyId, true);
    return !!cmd && isInteractiveSshSession(cmd);
  } catch {
    return false;
  }
}

/**
 * Build a shell script for background SSH execution.
 * This runs as a non-interactive command, not through the user's PTY.
 * Sets up: lockfile, MCP entry in ~/.claude.json, hooks in ~/.claude/settings.json.
 */
function buildSetupScript(
  remotePort: number,
  authToken: string,
  tabId: string,
  scripts: commands.MaitermSkillScripts,
  sharedHost: boolean,
): string {
  const lockContent = JSON.stringify({
    pid: 0,  // Background SSH — no persistent PID on remote
    transport: 'ws',
    authToken,
    serverPort: remotePort,
    ideName: 'maiTerm',
    ideVersion: '1.0',
    workspaceFolders: [],
  });

  // Escape single quotes for shell
  const escapedLockContent = lockContent.replace(/'/g, "'\\''");

  // MCP entry for ~/.claude.json registration. EVERY VALUE IS UNEXPANDED ON PURPOSE.
  //
  // ~/.claude.json holds ONE `mcpServers.maiterm` entry per remote ACCOUNT, shared by every
  // tab bridged to that host — and, since nothing arbitrates it, by every maiTerm on every
  // machine that bridges to that account. Any concrete value here belongs to whoever wrote
  // last: a baked tab id hands one tab's identity to its siblings, and a baked port and token
  // point the whole account at one instance's tunnel, so when that tunnel goes, so does
  // everyone's MCP.
  //
  // Written as placeholders instead, the entry is the same bytes from every instance and
  // overwriting it is a no-op. Each agent resolves it from its own shell environment, which
  // is per-tab and cannot be clobbered by anyone. Verified on a live remote: the `url`
  // expands as well as the headers, the connection uses the expanded port (not just the
  // display), and a wrong `${MAITERM_AUTH}` is rejected — so both are really in play.
  //
  // The cost is that an env-less shell (tmux, su, an ssh the user typed themselves) sends the
  // literal placeholder and cannot connect at all, where before it reached whichever maiTerm
  // wrote last. That is a downgrade only in appearance: on a contended account the port it
  // used to reach was frequently a dead one belonging to another machine.
  const mcpEntry = JSON.stringify({
    type: 'sse',
    url: 'http://127.0.0.1:${MAITERM_PORT}/sse',
    headers: {
      'x-claude-code-ide-authorization': '${MAITERM_AUTH}',
      'x-maiterm-tab': '${MAITERM_TAB_ID}',
    },
  });
  // Escape for single-quoted shell string
  const escapedMcpEntry = mcpEntry.replace(/'/g, "'\\''");

  // ── Hooks registration ──
  // Build hooks data for ~/.claude/settings.json on the remote. Like the MCP entry above,
  // this file is a per-ACCOUNT singleton, so every hook here is a COMMAND hook reading
  // $MAITERM_PORT / $MAITERM_AUTH / $MAITERM_TAB_ID rather than an http hook naming a port.
  //
  // Command hooks are the only kind that can: an http hook's url and headers do NOT expand
  // ${VAR} (a header resolves to an empty string — silent and destructive), which is why the
  // hooks half of this file looked unfixable. A command hook runs in the tab's own shell and
  // simply reads the environment, so the file becomes the same bytes from every instance.
  //
  // Two things fall out. `?tab_id=` is now on EVERY event, not just SessionStart — an http
  // hook could never carry it — so events no longer have to be matched to a tab through the
  // pending pool. And there are no http hooks left to allow, so `allowedHttpHookUrls` (which
  // was itself contended, being a single list each instance rewrote) disappears.
  //
  // Kept in the foreground rather than backgrounded with `&`: the server returns nothing for
  // these events, so a reply is not what we are waiting for, but ORDER matters — tab state is
  // derived from the sequence (UserPromptSubmit → PreToolUse → PostToolUse → Stop), and
  // detached curls can finish out of order. The cost over an http hook is a fork, against a
  // POST both kinds have to make anyway.
  const hookPost =
    "curl -s -o /dev/null --connect-timeout 2 --max-time 4 " +
    "-H \"x-claude-code-ide-authorization: $MAITERM_AUTH\" -H 'content-type: application/json' ";
  const hooksUrlExpr = "\"http://127.0.0.1:$MAITERM_PORT/hooks?tab_id=$MAITERM_TAB_ID\"";

  // Every hook starts the same way: recover the identity from ~/.aiterm when the shell has
  // none (tmux/su), then fall through silently unless we know BOTH which maiTerm to reach and
  // which tab is asking. Without the port there is nowhere to send it; without the tab id the
  // event cannot be attributed, and a misattributed event is worse than a missing one.
  const hookGate =
    "{ [ -z \"$MAITERM_TAB_ID\" ] && [ -f ~/.aiterm ] && . ~/.aiterm; } 2>/dev/null; " +
    "[ -n \"$MAITERM_PORT\" ] && [ -n \"$MAITERM_TAB_ID\" ] && { ";

  // The generic event hook: forward stdin verbatim, ignore the reply.
  // `--data-binary @-` streams the payload straight through — nothing here needs to read it.
  const eventCmd = hookGate + hookPost + "--data-binary @- " + hooksUrlExpr + " 2>/dev/null; } || true";

  // SessionStart is the one event whose REPLY matters, so it cannot use the generic hook: it
  // asks for `&prime=1` and echoes what comes back, which is how the standing instructions the
  // server tailors to this tab reach the model. Everything else gets StatusCode::OK and an
  // empty body.
  //
  // It captures stdin ONCE (`cat` is not re-readable) and re-uses it for both the session-id
  // extraction and the POST. Mirrors the local hook in lockfile.rs build_our_hooks — keep the
  // two in step. --max-time is mandatory here above all: this URL IS a reverse tunnel, and a
  // zombie tunnel port accepts the connect then never answers.
  // NOTE: no apostrophes inside the single-quoted echo string — one would close the quote.
  // Uses double-quoted JS strings so `${}` is not read as template interpolation.
  const sessionStartCmd =
    hookGate +
    "MAITERM_IN=$(cat); " +
    "MAITERM_SID=$(printf '%s' \"$MAITERM_IN\" | sed -n 's/.*\"session_id\" *: *\"\\([^\"]*\\)\".*/\\1/p' | head -1); " +
    "MAITERM_PRIME=$(curl -s --connect-timeout 2 --max-time 4 " +
    "-H \"x-claude-code-ide-authorization: $MAITERM_AUTH\" -H 'content-type: application/json' " +
    "--data-binary \"$MAITERM_IN\" " +
    "\"http://127.0.0.1:$MAITERM_PORT/hooks?tab_id=$MAITERM_TAB_ID&prime=1\" 2>/dev/null); " +
    "echo 'Your maiTerm tab ID is '$MAITERM_TAB_ID'. Your session ID is '$MAITERM_SID'. " +
    "maiTerm already knows this tab and session; you do NOT need to initialize. Only if a maiTerm tool answers that it does not know your tab, call the maiterm initSession tool with this tabId and sessionId to re-bind.'\"$MAITERM_PRIME\"; " +
    "} || true";

  // SessionEnd needs no special case any more. It used to be the only other hook that ran in
  // the tab's shell, because only such a hook can say WHICH tab ended — the thing that lets the
  // server clear the right mapping when a reload clone shares the original's session id. Now
  // every event carries its tab, so the generic hook says it for all of them.
  // NOTE: keep all of these pure ASCII (they are decoded by the remote python3 under the ssh locale).
  const commandHook = (command: string) => ({ matcher: "", hooks: [{ type: "command", command, timeout: 5 }] });
  const eventHook = commandHook(eventCmd);

  const hooksData = JSON.stringify({
    hooks: {
      SessionStart: [commandHook(sessionStartCmd)],
      SessionEnd: [eventHook],
      Notification: [eventHook],
      Stop: [eventHook],
      UserPromptSubmit: [eventHook],
      PreToolUse: [eventHook],
      PostToolUse: [eventHook],
      PreCompact: [eventHook],
    },
  });
  const escapedHooksData = hooksData.replace(/'/g, "'\\''");

  // Python script to merge hooks into ~/.claude/settings.json.
  // Removes ALL maiTerm-related hook entries (stale or current), then adds only ours.
  //
  // That sweep used to be how one instance destroyed another's config rather than merely
  // overwriting it. It is safe now for the reason the rest of this file is: what we add back
  // is byte-identical to what any other maiTerm would add, so removing theirs and writing ours
  // leaves the file exactly as it was. It still earns its keep against hooks from OLD builds,
  // which named a port and are now dead weight.
  //
  // allowedHttpHookUrls is stripped and not rewritten: with no http hooks left there is
  // nothing to allow, and the list was itself a contended singleton — one array that every
  // instance rewrote with its own port. Drop the key when it empties rather than leaving [].
  // No single quotes in the python code (shell wraps it in single quotes).
  const pythonHooks =
    'import json,sys,os,re\n' +
    'h=json.load(sys.stdin)\n' +
    'p=os.path.expanduser("~/.claude/settings.json")\n' +
    's=json.load(open(p)) if os.path.exists(p) else {}\n' +
    'def is_aiterm(e):\n' +
    ' for hk in e.get("hooks",[]):\n' +
    '  u=hk.get("url","")\n' +
    '  if re.search(r"127\\.0\\.0\\.1:\\d+/hooks",u):return True\n' +
    '  if hk.get("type")=="command" and "AITERM" in hk.get("command",""):return True\n' +
    ' return False\n' +
    'for ev,entries in h["hooks"].items():\n' +
    ' existing=[e for e in s.get("hooks",{}).get(ev,[]) if not is_aiterm(e)]\n' +
    ' existing.extend(entries)\n' +
    ' s.setdefault("hooks",{})[ev]=existing\n' +
    'a=[u for u in s.get("allowedHttpHookUrls",[]) if not re.search(r"127\\.0\\.0\\.1:\\d+/hooks",u)]\n' +
    'if a:\n' +
    ' s["allowedHttpHookUrls"]=a\n' +
    'else:\n' +
    ' s.pop("allowedHttpHookUrls",None)\n' +
    'open(p,"w").write(json.dumps(s,indent=2))';

  // Build script with newline separators (semicolons after `do`/`then`/`else` are syntax errors).
  // All JSON data is passed via shell variables to avoid quoting issues with python/jq.
  const script = [
    // Store JSON in shell variables to avoid nested quote hell
    `__lock='${escapedLockContent}'`,
    `__mcp='${escapedMcpEntry}'`,
    `__hooks='${escapedHooksData}'`,
    // Stale lockfile cleanup — uses curl to verify the server responds with HTTP.
    // /dev/tcp is unreliable with ControlMaster: dead tunnels appear alive because
    // the master keeps old port forwardings open even after the bridge process exits.
    'for __f in ~/.claude/ide/*.lock; do',
    '[ -f "$__f" ] || continue',
    'grep -q aiTerm "$__f" 2>/dev/null || continue',
    '__p=$(grep -o \'"serverPort":[0-9]*\' "$__f" 2>/dev/null | grep -o \'[0-9]*\')',
    '__t=$(grep -o \'"authToken":"[^"]*"\' "$__f" 2>/dev/null | cut -d\'"\'  -f4)',
    '[ -n "$__p" ] && [ "$__p" != "' + remotePort + '" ] && {',
    // --max-time bounds the TOTAL request: a zombie reverse-tunnel port (kept
    // listening by a dead ControlMaster) accepts the TCP connect but never answers,
    // so --connect-timeout alone lets curl block on the response read forever —
    // which hangs this whole setup script until ssh_run_setup's 30s timeout, wedging
    // the bridge in 'failed'. With --max-time it returns 000 → the lock is pruned.
    '__code=$(curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 --max-time 4 -X POST -H "x-claude-code-ide-authorization: $__t" "http://127.0.0.1:$__p/hooks" 2>/dev/null)',
    '[ "$__code" = "000" ] || [ "$__code" = "" ] && rm -f "$__f"',
    '} 2>/dev/null',
    'done',
    // Write lockfile
    'mkdir -p ~/.claude/ide',
    `printf '%s' "$__lock" > ~/.claude/ide/${remotePort}.lock`,
    // Register MCP in ~/.claude.json + hooks in ~/.claude/settings.json
    'if command -v python3 >/dev/null 2>&1; then',
    'printf \'%s\' "$__mcp" | python3 -c \'import json,sys,os; e=json.load(sys.stdin); p=os.path.expanduser("~/.claude.json"); d=json.load(open(p)) if os.path.exists(p) else {}; m=d.setdefault("mcpServers",{}); m["maiterm"]=e; m.pop("aiterm",None); open(p,"w").write(json.dumps(d,indent=2))\'',
    "printf '%s' \"$__hooks\" | python3 -c '" + pythonHooks + "'",
    'elif command -v jq >/dev/null 2>&1; then',
    '[ -f ~/.claude.json ] || echo \'{}\' > ~/.claude.json',
    'jq --argjson entry "$__mcp" \'.mcpServers.maiterm = $entry | del(.mcpServers.aiterm)\' ~/.claude.json > ~/.claude.json.tmp && mv ~/.claude.json.tmp ~/.claude.json',
    'else',
    '[ -f ~/.claude.json ] || echo \'{}\' > ~/.claude.json',
    'fi',
    // Per-ACCOUNT fallback for env-less shells (tmux/su) so hooks still know their
    // tab. The file is shared by every bridge to this account: on a host where this
    // maiTerm has multiple bridged tabs it would hand a sibling tab's identity to any
    // env-less agent (session/tab identity cross-pollution), so write it only when this
    // tab is the sole bridge to the host; on shared hosts remove it (also scrubs stale
    // pre-fix files) and let those agents fail closed to a visible "needs init".
    // 0600: it now carries the auth token as well as the identity, and the account may be
    // shared with other humans on the box.
    sharedHost
      ? 'rm -f ~/.aiterm'
      : `printf 'export MAITERM_TAB_ID=${tabId}\\nexport MAITERM_PORT=${remotePort}\\nexport MAITERM_AUTH=${authToken}\\n' > ~/.aiterm && chmod 600 ~/.aiterm`,
    // Install /maiterm skill on the remote (drop any legacy /aiterm one)
    'rm -rf ~/.claude/skills/aiterm',
    'mkdir -p ~/.claude/skills/maiterm',
    "cat > ~/.claude/skills/maiterm/SKILL.md << 'SKILLEOF'\n" +
    // Single source of truth: the SKILL.md body comes from the bundled resource
    // (get_maiterm_skill_scripts), identical to the local install — no drift.
    scripts.skill_md +
    'SKILLEOF',
    // Bundle the /maiterm statusline helper scripts on the remote too, so
    // `/maiterm statusline` works in remote (SSH-bridged) Claude sessions.
    'mkdir -p ~/.claude/skills/maiterm/bin',
    "cat > ~/.claude/skills/maiterm/bin/setup-statusline.sh << 'MAITERMSETUPEOF'",
    scripts.setup_statusline,
    'MAITERMSETUPEOF',
    "cat > ~/.claude/skills/maiterm/bin/statusline-command.sh << 'MAITERMPAYLOADEOF'",
    scripts.statusline_command,
    'MAITERMPAYLOADEOF',
    'chmod +x ~/.claude/skills/maiterm/bin/setup-statusline.sh ~/.claude/skills/maiterm/bin/statusline-command.sh',
  ];

  return script.join('\n');
}

/** In-flight enableBridge attempts, keyed by tab. */
const inFlightBridges = new Map<string, Promise<boolean>>();

/** Bumped on every teardown so an in-flight setup can tell it was superseded. */
const bridgeEpoch = new Map<string, number>();

/**
 * Enable the MCP bridge for an SSH tab.
 * Spawns (or reuses) a reverse tunnel, writes lockfile + hooks via background SSH,
 * and injects MAITERM_TAB_ID / MAITERM_PORT env vars into the remote shell.
 *
 * Concurrent calls for the same tab JOIN the in-flight attempt. That's load-bearing
 * for ORDERING, not just efficiency: the restore path awaits this and then sends
 * `claude --resume …`. A tab coming back after a restart gets two callers — the
 * restore poll and the title event fired by the remote's login prompt — and whichever
 * lost the race used to return instantly on the other's 'pending' status. The resume
 * then fired while the injection was still pending, so the export landed as literal
 * text inside the agent's TUI instead of its shell. Joining makes "bridge is up (and
 * injected)" a real precondition for everything sequenced after it.
 *
 * @param ptyId — if provided, the PTY the tab owns.
 * @param freshSsh — the caller OBSERVED this ssh session start (a non-ssh foreground, then
 *   ssh), so the remote end is a shell sitting at a prompt. Only then may env vars be typed
 *   into the live PTY. Without it we could be writing into whatever has taken the remote shell
 *   over since — which, on an agent tab, means the export appears in the agent's chat and the
 *   trailing newline sends it. maiTerm-initiated sessions pass `false`: buildSshCommand already
 *   baked the tab id into their remote command, so there is nothing to type.
 */
export function enableBridge(tabId: string, sshArgs: string, ptyId?: string, freshSsh = false, bakedPort?: number): Promise<boolean> {
  const inflight = inFlightBridges.get(tabId);
  if (inflight) return inflight;
  const attempt = enableBridgeInner(tabId, sshArgs, ptyId, freshSsh, bakedPort).finally(() => {
    inFlightBridges.delete(tabId);
  });
  inFlightBridges.set(tabId, attempt);
  return attempt;
}

async function enableBridgeInner(tabId: string, sshArgs: string, ptyId?: string, freshSsh = false, bakedPort?: number): Promise<boolean> {
  // Independent per-runtime gates: the tunnel + env injection are runtime-agnostic and
  // run for either; the remote setup writes Claude artifacts only when claudeOn and
  // Codex artifacts only when codexOn (so a Claude-only or Codex-only host both work).
  const claudeOn = preferencesStore.claudeCodeIde && preferencesStore.claudeCodeIdeSsh;
  const codexOn = preferencesStore.codexIde && preferencesStore.codexIdeSsh;
  if (!claudeOn && !codexOn) {
    return false;
  }

  // Strip leading "ssh " prefix — callers may pass the full ps command or just the args
  sshArgs = sshArgs.replace(/^ssh\s+/, '');

  // Already bridged or in progress? A prior 'failed' attempt is NOT a dead end —
  // fall through and retry it (the remote may have been briefly down, e.g. a network
  // blip during a reload). Only 'connected'/'pending' short-circuit. A tab parked as
  // 'reconnecting' (its tunnel died; see queueReconnect) is retried the same way — by
  // the scheduler on its backoff, or sooner by the title loop if the remote speaks first.
  const existing = bridgeStates.get(tabId);
  const retrying = existing?.status === 'failed' || existing?.status === 'reconnecting';
  if (existing && !retrying) return existing.status === 'connected';

  // Mark as pending immediately to prevent concurrent calls from racing
  const hostKey = extractHostKey(sshArgs);
  bridgeStates = new Map(bridgeStates.set(tabId, { hostKey, remotePort: 0, status: 'pending', ptyId }));

  // Snapshot the tab's teardown epoch. A disableBridge() landing while we're still
  // setting up must win: without this, our completion would re-register 'connected'
  // for a tab that has since logged out (and whose tunnel refcount was already
  // decremented), and that resurrected state then blocks the next bridge attempt.
  const epoch = bridgeEpoch.get(tabId) ?? 0;

  try {
    // Inside the try: these can REJECT, not just return null, and a throw before the
    // catch would strand the 'pending' status above forever — permanently blocking
    // both the retry branch and hostChanged, which only act on non-pending states.
    const localPort = await commands.getMcpPort();
    const authToken = await commands.getMcpAuth();
    if (!localPort || !authToken) {
      logError('Cannot enable SSH MCP bridge: MCP server not running');
      bridgeStates.delete(tabId);
      bridgeStates = new Map(bridgeStates);
      return false;
    }

    // Start or join existing tunnel
    const tunnelInfo = await commands.startSshTunnel(sshArgs, hostKey, tabId, localPort);
    logInfo(`SSH MCP bridge: tunnel to ${hostKey} on remote port ${tunnelInfo.remote_port}`);

    // Set trigger variables so auto-resume commands can interpolate them.
    // %maitermTabId, %maitermPort for individual values, %maitermExport for the full export command.
    setVariable(tabId, 'maitermTabId', tabId);
    setVariable(tabId, 'maitermPort', String(tunnelInfo.remote_port));
    setVariable(tabId, 'maitermExport',
      `export MAITERM_TAB_ID=${tabId} MAITERM_PORT=${tunnelInfo.remote_port} MAITERM_AUTH=${authToken}`);

    // Inject MAITERM_TAB_ID and MAITERM_PORT into the remote shell FIRST — before
    // building or kicking off the remote setup below. The injection only needs
    // remote_port (already known), so NOTHING else should sit between tunnel-up
    // and the PTY write: every await in between (getMaitermSkillScripts,
    // buildCodexSetupScript, and the foreground probe) delays the export landing
    // at the remote prompt and lets the user's first keystrokes race in front of
    // it. That race is exactly what building the setup scripts here used to cause.
    // Leading space suppresses shell history (bash HISTCONTROL=ignorespace, zsh
    // HIST_IGNORE_SPACE). Re-check the foreground process right before writing:
    // tunnel setup is async (~1-2s), and the ssh process may have exited in the
    // meantime (quick user disconnect, one-shot command that slipped past the
    // filter, etc.). Writing to a PTY whose foreground is no longer ssh dumps the
    // export into the local shell. The probe is foreground-only (no lsof) to keep
    // this last gate as thin as possible.
    // A maiTerm-initiated session baked the port into its own ssh command, and that value is
    // a GUESS — this maiTerm's usual port on the host, read before the tunnel existed. When
    // the guess missed, the shell is pointed at a port serving nothing (or, worse, a sibling
    // account's tunnel), and the failure is silent: the tab looks connected and its agent
    // simply has no MCP and no hooks. So a stale bake overrides the "we baked it, there is
    // nothing to type" rule below — that rule was written when the tab id was the only thing
    // baked, and a tab id, unlike a port, is always right.
    //
    // `bakedPort` is an ARGUMENT and not something looked up here, which is the safety
    // property: only a caller still holding the remote shell may ask for a correction. The
    // agent guard below cannot be relied on to catch the difference, because it is
    // ANTI-correlated with staleness — a wrong port is exactly what stops this tab's hooks
    // reaching us, so `agentStateStore` is empty precisely when an agent is running. Callers
    // that hand the PTY to an agent in the same breath as the ssh command (the auto-resume
    // replay in triggers.svelte.ts) therefore pass nothing, and the title-driven and manual
    // re-bridges below cannot pass anything at all.
    const bakedIsStale = bakedPort !== undefined && bakedPort !== tunnelInfo.remote_port;
    if (ptyId && injectedEnvPort.get(tabId) === tunnelInfo.remote_port) {
      // Already injected for this port — a prior attempt's export is still live in
      // the shell. Re-injecting on every failed-setup retry would spam the user's
      // interactive session with `export MAITERM_TAB_ID=…` lines, once per prompt.
      logInfo("SSH MCP bridge: env vars already injected for tab " + tabId + " — skipping re-injection");
    } else if (ptyId && !freshSsh && !bakedIsStale) {
      // Not a shell we watched connect, so we cannot know what owns the remote end now. The
      // live-agent check below is negative evidence, and it is blindest exactly when it
      // matters: re-bridging a tab whose agent has gone quiet is the moment its session
      // mapping is missing, so the guard sees nothing and the export lands in the chat. The
      // tab id still reaches the remote through the baked ssh command (maiTerm-initiated
      // sessions), ~/.aiterm (sole-tab hosts), or the manual "Inject maiTerm Env Vars" action.
      logInfo("SSH MCP bridge: skipping env-var injection — ssh session for tab " + tabId + " was not observed starting, so the remote shell may not be at a prompt");
    } else if (ptyId) {
      if (bakedIsStale) {
        logInfo("SSH MCP bridge: tab " + tabId + " baked MAITERM_PORT=" + bakedPort
          + " but the tunnel came up on " + tunnelInfo.remote_port + " — correcting the remote shell");
      }
      try {
        if (!(await isRemoteShellForeground(ptyId))) {
          logInfo("SSH MCP bridge: skipping env-var injection — ssh no longer foreground for tab " + tabId);
        } else if (agentStateStore.getState(tabId)) {
          // An agent session is already live in this tab, so the PTY belongs to its
          // prompt, not a shell: the write would be typed into the agent as a message
          // (and the trailing newline would submit it). Skipping loses nothing — an
          // export cannot change the environment of an ALREADY-RUNNING process — while
          // the remote ~/.aiterm written by the background setup ssh still carries the
          // tab id for the SessionStart hook to source.
          //
          // Deliberately NOT keyed on the alternate screen: Claude Code renders on the
          // PRIMARY screen (that's why width changes duplicate its transcript into
          // scrollback), so alt-screen misses the agent this is meant to protect, while
          // catching tmux — where writing is fine, because tmux forwards keystrokes to
          // the inner shell, and where the export is most needed since tmux shells
          // don't inherit the spawn env.
          //
          // When the bake was stale this is the one case the correction cannot reach: the
          // agent has already read the wrong port out of its environment, and only a restart
          // of that agent will pick up the right one. Say so, because the symptom at the
          // other end is an MCP server that will not connect for the session's whole life.
          logInfo("SSH MCP bridge: skipping env-var injection — an agent session owns tab " + tabId
            + (bakedIsStale ? " (its MAITERM_PORT is stale; the agent must be restarted to pick up " + tunnelInfo.remote_port + ")" : ""));
        } else {
          const envCmd = " export MAITERM_TAB_ID=" + tabId + " MAITERM_PORT=" + tunnelInfo.remote_port
            + " MAITERM_AUTH=" + authToken + "\n";
          const bytes = Array.from(new TextEncoder().encode(envCmd));
          await commands.writeTerminal(ptyId, bytes);
          injectedEnvPort.set(tabId, tunnelInfo.remote_port);
          logInfo("SSH MCP bridge: injected env vars into remote shell for tab " + tabId);
        }
      } catch (e) {
        logError("SSH MCP bridge: failed to inject env vars: " + e);
      }
    }

    // Now kick off remote setup(s) — these gate the 'connected' status flip but
    // NOT the interactive experience, so they run after the injection above rather
    // than in front of it. Claude and Codex are SEPARATE background-SSH setups
    // behind independent gates, so one failing can't break the other.
    const setupPromises: Promise<void>[] = [];
    if (claudeOn) {
      const skillScripts = await commands.getMaitermSkillScripts();
      const setupScript = buildSetupScript(
        tunnelInfo.remote_port, authToken, tabId, skillScripts, isSharedHost(hostKey, tabId));
      setupPromises.push(commands.sshRunSetup(sshArgs, setupScript));
    }
    if (codexOn) {
      // No-ops on a remote without the codex CLI. Reuses the local CodexRegistrar's
      // renderers (config.toml + hooks.json + shim + prompt), pointed at the tunnel port.
      const codexScript = await commands.buildCodexSetupScript(
        tunnelInfo.remote_port, authToken, tabId, preferencesStore.codexHooks);
      setupPromises.push(commands.sshRunSetup(sshArgs, codexScript));
    }

    // Deshittification rides along on every connect, in both directions: rules on
    // here are applied there, rules off here are removed there. It is deliberately
    // NOT in setupPromises — it is a preference, not a dependency, so it must never
    // gate 'connected' or fail a bridge. Idle when the user has enabled nothing.
    void (async () => {
      try {
        const script = await commands.buildDeshittifySetupScript();
        if (script.trim()) await commands.sshRunSetup(sshArgs, script);
      } catch (e) {
        logError("SSH MCP bridge: deshittification setup failed: " + e);
      }
    })();

    // Wait for remote setup(s) to finish before flipping to 'connected'.
    // If any setup failed, this throws and the outer catch marks the bridge as failed.
    await Promise.all(setupPromises);

    // A teardown raced us (user logged out mid-setup) — don't resurrect the bridge.
    if ((bridgeEpoch.get(tabId) ?? 0) !== epoch) {
      logInfo("SSH MCP bridge: discarding setup result for tab " + tabId + " — bridge was disabled while connecting");
      return false;
    }

    bridgeStates = new Map(bridgeStates.set(tabId, {
      hostKey,
      remotePort: tunnelInfo.remote_port,
      status: 'connected',
      ptyId,
    }));

    // Listen for tunnel process death from Rust — clears indicator in real-time
    listenForTunnelDown(tabId).catch(() => {});

    logInfo(`SSH MCP bridge enabled for tab ${tabId} → ${hostKey}:${tunnelInfo.remote_port}`);
    return true;
  } catch (e) {
    const errMsg = String(e);
    logError(`SSH MCP bridge failed for ${hostKey}: ${errMsg}`);

    bridgeStates = new Map(bridgeStates.set(tabId, {
      hostKey,
      remotePort: 0,
      status: 'failed',
      error: errMsg,
      ptyId,
    }));

    // Only surface the toast on the first failure of an episode. Retries — the term-title
    // loop once the host recovers, or the reconnect scheduler on its backoff — that fail
    // again shouldn't re-nag; the scheduler speaks once, when it gives up.
    if (!retrying) {
      dispatch('MCP Bridge Failed', `Could not connect to ${hostKey}: ${errMsg}`, 'error', { tabId });
    }
    return false;
  }
}

/**
 * Disable the MCP bridge for a tab (called on tab close or SSH disconnect).
 */
export async function disableBridge(tabId: string): Promise<void> {
  // Bump first and unconditionally: a setup may be in flight with no state to read
  // yet, and it must still see that a teardown happened.
  bridgeEpoch.set(tabId, (bridgeEpoch.get(tabId) ?? 0) + 1);

  const bridge = bridgeStates.get(tabId);
  if (!bridge) return;

  cleanupListener(tabId);
  forgetDownTab(tabId);
  bridgeStates.delete(tabId);
  bridgeStates = new Map(bridgeStates);
  injectedEnvPort.delete(tabId);

  try {
    await commands.detachSshTunnel(bridge.hostKey, tabId);
  } catch (e) {
    logError(`Failed to detach SSH tunnel: ${e}`);
  }
}

/**
 * Check if a tab has an active MCP bridge.
 */
export function hasBridge(tabId: string): boolean {
  return bridgeStates.has(tabId);
}

/**
 * Get bridge status for a tab (reactive).
 */
export function getBridgeStatus(tabId: string): BridgeStatus | undefined {
  return bridgeStates.get(tabId)?.status;
}

/**
 * Get bridge info for a tab.
 */
export function getBridgeInfo(tabId: string): BridgeState | undefined {
  return bridgeStates.get(tabId);
}

/**
 * Build the full setup script for the current user's home directory.
 * Used by "Install MCP for Current User" context menu item when the user
 * has done `sudo -i` or `su -l otheruser` and needs the config files
 * written to that user's ~/ instead of the original SSH user's.
 */
export async function buildUserSetupScript(tabId: string): Promise<string | null> {
  const bridge = bridgeStates.get(tabId);
  if (!bridge || bridge.status !== 'connected' || !bridge.remotePort) return null;

  const authToken = await commands.getMcpAuth();
  if (!authToken) return null;

  const claudeOn = preferencesStore.claudeCodeIde && preferencesStore.claudeCodeIdeSsh;
  const codexOn = preferencesStore.codexIde && preferencesStore.codexIdeSsh;

  // Concatenate the enabled runtimes' setup scripts (run in the user's shell after
  // sudo/su). The Codex block is guarded by `if command -v codex` (not `exit`), so it
  // is a safe no-op in the interactive shell when codex isn't installed for this user.
  const parts: string[] = [];
  if (claudeOn) {
    const skillScripts = await commands.getMaitermSkillScripts();
    parts.push(buildSetupScript(
      bridge.remotePort, authToken, tabId, skillScripts, isSharedHost(bridge.hostKey, tabId)));
  }
  if (codexOn) {
    parts.push(await commands.buildCodexSetupScript(
      bridge.remotePort, authToken, tabId, preferencesStore.codexHooks));
  }
  // Runtime-independent, and safe in an interactive shell: it ends in `:`, not
  // `exit`, and every branch is guarded.
  if (parts.length) {
    const desh = await commands.buildDeshittifySetupScript();
    if (desh.trim()) parts.push(desh);
  }
  return parts.length ? parts.join('\n') : null;
}
