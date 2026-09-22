// Workspace Share, frontend half (docs/workspace-share.md). The file format, git and the
// workspace build live in Rust (src-tauri/src/share/); this module owns what only the webview
// knows — live tab context going out, agent launch commands coming in.

import type { Workspace } from '$lib/tauri/types';
import type { ShareAgentLaunch, ShareTabContext } from '$lib/tauri/commands';
import { cleanSshCommand, getPtyInfo } from '$lib/tauri/commands';
import { terminalsStore, type SplitContext } from '$lib/stores/terminals.svelte';
import { agentStateStore } from '$lib/stores/agentState.svelte';
import { launchCommand } from '$lib/agents/descriptor';
import { buildForkCommand } from '$lib/agents/resume';

/** Where each terminal tab is right now, with every ssh command CLEANED: the raw one carries
 *  maiTerm's injected remote command, and with it the sender's tab id and MCP auth token. */
export async function gatherShareContexts(ws: Workspace): Promise<ShareTabContext[]> {
  const out: ShareTabContext[] = [];
  for (const pane of ws.panes) {
    for (const tab of pane.tabs) {
      if ((tab.tab_type ?? 'terminal') !== 'terminal') continue;
      let cwd: string | null = null;
      let ssh: string | null = null;
      let remoteCwd: string | null = null;
      const inst = terminalsStore.get(tab.id);
      if (inst) {
        try {
          const info = await getPtyInfo(inst.ptyId);
          cwd = info.cwd;
          if (info.foreground_command) {
            ssh = cleanSshCommand(info.foreground_command);
            const osc = terminalsStore.getOsc(tab.id);
            const osc7 = osc?.cwd ?? null;
            // An OSC 7 equal to the local cwd is the local shell's, left over from before ssh.
            remoteCwd = (osc7 && osc7 !== cwd ? osc7 : null) ?? osc?.promptCwd ?? null;
          }
        } catch { /* PTY gone — fall back to the persisted context below */ }
      }
      const persistedSsh = tab.auto_resume_ssh_command ?? tab.restore_ssh_command ?? null;
      if (!ssh && !inst && persistedSsh) ssh = cleanSshCommand(persistedSsh);
      const live = agentStateStore.getState(tab.id);
      out.push({
        tab_id: tab.id,
        cwd: ssh ? null : cwd,
        ssh_command: ssh,
        remote_cwd: remoteCwd,
        live_agent: live ? { runtime: live.runtime, session_id: live.sessionId || null } : null,
      });
    }
  }
  return out;
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** The one-shot start for an imported agent tab (§5): a fork of the recorded remote session,
 *  falling back to a fresh agent when the receiver can't read it, or just a fresh agent.
 *
 *  The fallback pattern names THE session id. A forked session replays its transcript, and a
 *  transcript about this very feature can contain "No conversation found"; one containing that
 *  phrase next to this exact id is not a thing that happens by accident. */
export function launchContextFor(l: ShareAgentLaunch): SplitContext {
  const fresh = launchCommand(l.runtime);
  const fork = l.fork_session_id ? buildForkCommand(l.runtime, l.fork_session_id) : null;
  const id = l.fork_session_id ? escapeRe(l.fork_session_id) : '';
  return {
    cwd: l.cwd,
    sshCommand: l.ssh_command,
    remoteCwd: l.remote_cwd,
    launchCommand: fork ?? fresh,
    launchFallback: fork
      ? {
          // Claude Code 2.1.280: "No conversation found with session ID: <id>". Codex's wording is
          // unverified, so match any not-found shape that names the id.
          pattern: new RegExp(`(?:no conversation found|not found|no such|does not exist|couldn't find|could not find)[^\\n]{0,80}${id}|${id}[^\\n]{0,80}(?:not found|does not exist)`, 'i'),
          command: fresh,
          withinMs: 60_000,
        }
      : undefined,
  };
}

export const SHARE_EXTENSION = 'maiterm-workspace';
