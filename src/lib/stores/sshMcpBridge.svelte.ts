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
import { countedListen as listen } from '$lib/utils/listenCounter';
import type { UnlistenFn } from '@tauri-apps/api/event';

export type BridgeStatus = 'connected' | 'pending' | 'failed';

interface BridgeState {
  hostKey: string;
  remotePort: number;
  status: BridgeStatus;
  error?: string;
}

/** Reactive map of tabId → bridge state. Svelte 5 $state for reactivity in TerminalTabs. */
let bridgeStates = $state<Map<string, BridgeState>>(new Map());

/** Per-tab event listeners for tunnel-down events from Rust. */
const tunnelListeners = new Map<string, UnlistenFn>();

/**
 * Remove bridge state for a tab (internal — no backend call).
 * Used when Rust notifies us the tunnel died.
 */
function clearBridgeState(tabId: string): void {
  if (!bridgeStates.has(tabId)) return;
  bridgeStates.delete(tabId);
  bridgeStates = new Map(bridgeStates);
  logInfo(`SSH MCP bridge cleared for tab ${tabId} (tunnel down)`);
}

/**
 * Start listening for tunnel-down events from Rust for this tab.
 */
async function listenForTunnelDown(tabId: string): Promise<void> {
  // Already listening?
  if (tunnelListeners.has(tabId)) return;
  const unlisten = await listen(`ssh-tunnel-down-${tabId}`, () => {
    logInfo(`Received ssh-tunnel-down for tab ${tabId}`);
    clearBridgeState(tabId);
    cleanupListener(tabId);
  });
  tunnelListeners.set(tabId, unlisten);
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
 * Build a shell script for background SSH execution.
 * This runs as a non-interactive command, not through the user's PTY.
 * Sets up: lockfile, MCP entry in ~/.claude.json, hooks in ~/.claude/settings.json.
 */
function buildSetupScript(
  remotePort: number,
  authToken: string,
  tabId: string,
  scripts: commands.MaitermSkillScripts,
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

  // MCP entry for ~/.claude.json registration
  const mcpEntry = JSON.stringify({
    type: 'sse',
    url: `http://127.0.0.1:${remotePort}/sse`,
    headers: { 'x-claude-code-ide-authorization': authToken },
  });
  // Escape for single-quoted shell string
  const escapedMcpEntry = mcpEntry.replace(/'/g, "'\\''");

  // ── Hooks registration ──
  // Build hooks data for ~/.claude/settings.json on the remote.
  // HTTP hooks tunnel back through the reverse SSH tunnel to our local MCP server.
  const hooksUrl = "http://127.0.0.1:" + remotePort + "/hooks";

  // SessionStart command hook: reads $AITERM_TAB_ID (injected into PTY after bridge setup),
  // extracts session_id from hook stdin, echoes both into Claude's context.
  // Uses double-quoted JS string to avoid template literal ${} interpolation of bash vars.
  // SessionStart hook: reads $AITERM_TAB_ID from env, falls back to ~/.aiterm file
  // (needed when Claude runs inside tmux where env vars weren't inherited).
  const sessionStartCmd =
    "{ [ -z \"$AITERM_TAB_ID\" ] && [ -f ~/.aiterm ] && . ~/.aiterm; } 2>/dev/null; " +
    "{ [ \"$AITERM_PORT\" = \"" + remotePort + "\" ] || [ -z \"$AITERM_PORT\" ]; } && " +
    "[ -n \"$AITERM_TAB_ID\" ] && " +
    "AITERM_SID=$(cat | sed -n 's/.*\"session_id\" *: *\"\\([^\"]*\\)\".*/\\1/p' | head -1) && " +
    "echo 'Your maiTerm tab ID is '$AITERM_TAB_ID'. Your session ID is '$AITERM_SID'. " +
    "You MUST immediately call the maiterm initSession tool with this tabId and sessionId before responding to the user. This is required for correct tab targeting.' || true";

  const httpHook = { matcher: "", hooks: [{ type: "http", url: hooksUrl, headers: { "x-claude-code-ide-authorization": authToken } }] };

  const hooksData = JSON.stringify({
    url: hooksUrl,
    port: remotePort,
    hooks: {
      SessionStart: [
        { matcher: "", hooks: [{ type: "command", command: sessionStartCmd, timeout: 5 }] },
        httpHook,
      ],
      SessionEnd: [httpHook],
      Notification: [httpHook],
      Stop: [httpHook],
      UserPromptSubmit: [httpHook],
      PreToolUse: [httpHook],
      PostToolUse: [httpHook],
      PreCompact: [httpHook],
    },
  });
  const escapedHooksData = hooksData.replace(/'/g, "'\\''");

  // Python script to merge hooks into ~/.claude/settings.json.
  // Removes ALL maiTerm-related hook entries (stale or current), then adds only ours.
  // Stale hooks from dead tunnels (kept alive by ControlMaster) cause errors otherwise.
  // Also cleans up stale allowedHttpHookUrls from dead ports.
  // No single quotes in the python code (shell wraps it in single quotes).
  const pythonHooks =
    'import json,sys,os,re\n' +
    'h=json.load(sys.stdin)\n' +
    'p=os.path.expanduser("~/.claude/settings.json")\n' +
    's=json.load(open(p)) if os.path.exists(p) else {}\n' +
    'url=h["url"]\n' +
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
    'a.append(url)\n' +
    's["allowedHttpHookUrls"]=a\n' +
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
    '__code=$(curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 -X POST -H "x-claude-code-ide-authorization: $__t" "http://127.0.0.1:$__p/hooks" 2>/dev/null)',
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
    // Write tab ID + port to ~/.aiterm so tmux/new shells can source it
    `printf 'export AITERM_TAB_ID=${tabId}\\nexport AITERM_PORT=${remotePort}\\n' > ~/.aiterm`,
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

/**
 * Enable the MCP bridge for an SSH tab.
 * Spawns (or reuses) a reverse tunnel, writes lockfile + hooks via background SSH,
 * and injects AITERM_TAB_ID / AITERM_PORT env vars into the remote shell.
 *
 * @param ptyId — if provided, injects env vars into the remote shell via PTY write.
 *   Leading space prevents the command from appearing in shell history.
 */
export async function enableBridge(tabId: string, sshArgs: string, ptyId?: string): Promise<boolean> {
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

  // Already bridged or in progress?
  if (bridgeStates.has(tabId)) return bridgeStates.get(tabId)!.status === 'connected';

  // Mark as pending immediately to prevent concurrent calls from racing
  const hostKey = extractHostKey(sshArgs);
  bridgeStates = new Map(bridgeStates.set(tabId, { hostKey, remotePort: 0, status: 'pending' }));

  const localPort = await commands.getMcpPort();
  const authToken = await commands.getMcpAuth();
  if (!localPort || !authToken) {
    logError('Cannot enable SSH MCP bridge: MCP server not running');
    bridgeStates.delete(tabId);
    bridgeStates = new Map(bridgeStates);
    return false;
  }

  try {
    // Start or join existing tunnel
    const tunnelInfo = await commands.startSshTunnel(sshArgs, hostKey, tabId, localPort);
    logInfo(`SSH MCP bridge: tunnel to ${hostKey} on remote port ${tunnelInfo.remote_port}`);

    // Kick off remote setup in parallel with env-var injection below. The injection
    // only needs remote_port, which we already have — awaiting setup first delays the
    // export landing in the remote shell by ~0.5-2s, which collides with the user's
    // first keystrokes. Claude and Codex are SEPARATE background-SSH setups behind
    // independent gates, so one failing can't break the other.
    const setupPromises: Promise<void>[] = [];
    if (claudeOn) {
      const skillScripts = await commands.getMaitermSkillScripts();
      const setupScript = buildSetupScript(tunnelInfo.remote_port, authToken, tabId, skillScripts);
      setupPromises.push(commands.sshRunSetup(sshArgs, setupScript));
    }
    if (codexOn) {
      // No-ops on a remote without the codex CLI. Reuses the local CodexRegistrar's
      // renderers (config.toml + hooks.json + shim + prompt), pointed at the tunnel port.
      const codexScript = await commands.buildCodexSetupScript(tunnelInfo.remote_port, authToken, tabId);
      setupPromises.push(commands.sshRunSetup(sshArgs, codexScript));
    }

    // Set trigger variables so auto-resume commands can interpolate them.
    // %aitermTabId, %aitermPort for individual values, %aitermExport for the full export command.
    setVariable(tabId, 'aitermTabId', tabId);
    setVariable(tabId, 'aitermPort', String(tunnelInfo.remote_port));
    setVariable(tabId, 'aitermExport', `export AITERM_TAB_ID=${tabId} AITERM_PORT=${tunnelInfo.remote_port}`);

    // Inject AITERM_TAB_ID and AITERM_PORT into the remote shell so hooks can read them.
    // Leading space suppresses shell history (bash HISTCONTROL=ignorespace, zsh HIST_IGNORE_SPACE).
    // Re-check the foreground process right before writing: tunnel setup is async
    // (~1-2s), and the ssh process may have exited in the meantime (quick user
    // disconnect, one-shot command that slipped past the filter, etc.). Writing to a
    // PTY whose foreground is no longer ssh dumps the export into the local shell.
    if (ptyId) {
      try {
        const info = await commands.getPtyInfo(ptyId);
        if (!info.foreground_command || !isInteractiveSshSession(info.foreground_command)) {
          logInfo("SSH MCP bridge: skipping env-var injection — ssh no longer foreground for tab " + tabId);
        } else {
          const envCmd = " export AITERM_TAB_ID=" + tabId + " AITERM_PORT=" + tunnelInfo.remote_port + "\n";
          const bytes = Array.from(new TextEncoder().encode(envCmd));
          await commands.writeTerminal(ptyId, bytes);
          logInfo("SSH MCP bridge: injected env vars into remote shell for tab " + tabId);
        }
      } catch (e) {
        logError("SSH MCP bridge: failed to inject env vars: " + e);
      }
    }

    // Wait for remote setup(s) to finish before flipping to 'connected'.
    // If any setup failed, this throws and the outer catch marks the bridge as failed.
    await Promise.all(setupPromises);

    bridgeStates = new Map(bridgeStates.set(tabId, {
      hostKey,
      remotePort: tunnelInfo.remote_port,
      status: 'connected',
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
    }));

    dispatch('MCP Bridge Failed', `Could not connect to ${hostKey}: ${errMsg}`, 'error', { tabId });
    return false;
  }
}

/**
 * Disable the MCP bridge for a tab (called on tab close or SSH disconnect).
 */
export async function disableBridge(tabId: string): Promise<void> {
  const bridge = bridgeStates.get(tabId);
  if (!bridge) return;

  cleanupListener(tabId);
  bridgeStates.delete(tabId);
  bridgeStates = new Map(bridgeStates);

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
    parts.push(buildSetupScript(bridge.remotePort, authToken, tabId, skillScripts));
  }
  if (codexOn) {
    parts.push(await commands.buildCodexSetupScript(bridge.remotePort, authToken, tabId));
  }
  return parts.length ? parts.join('\n') : null;
}
