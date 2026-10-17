/**
 * The Windows Claude setup wizard (src-tauri/src/windows_setup.rs says why it exists: Git for
 * Windows is what Claude runs hooks with, maiTerm's hooks are POSIX shell, and the installer
 * leaves `claude` off PATH). This holds the last detection, the install running in a tab and
 * whether the wizard is open — a store rather than the modal, so an install is still watched
 * while the user is looking at its tab, and its fixes still land.
 */
import { error as logError } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { ClaudeSetupReport } from '$lib/tauri/types';
import { workspacesStore, navigateToTab } from './workspaces.svelte';
import { terminalsStore } from './terminals.svelte';
import { preferencesStore } from './preferences.svelte';

export type InstallKind = 'git' | 'claude' | 'both';

const GIT_WINGET = 'winget install --id Git.Git -e --source winget --accept-package-agreements --accept-source-agreements';
const CLAUDE_INSTALL = 'irm https://claude.ai/install.ps1 | iex';
export const GIT_DOWNLOAD_URL = 'https://git-scm.com/downloads/win';

/** Typed into whatever shell the new tab has, so it goes through powershell.exe: one line that
 *  works from PowerShell, cmd, Git Bash and (through interop) WSL. Claude's line is the official
 *  installer's own (code.claude.com/docs/en/setup). */
export function installCommand(kind: InstallKind): string {
  const steps = kind === 'git' ? [GIT_WINGET] : kind === 'claude' ? [CLAUDE_INSTALL] : [GIT_WINGET, CLAUDE_INSTALL];
  return `powershell.exe -NoProfile -Command "${steps.join('; ')}"`;
}

/** Everything a fully working setup needs that this report lacks. */
export function missing(r: ClaudeSetupReport) {
  return {
    git: !r.git_bash,
    claude: !r.claude,
    shell: !!r.git_bash && r.shell !== 'gitbash',
    path: !!r.claude && !r.claude_on_path,
    /** Claude Desktop's alias answers `claude` first. Only a PATH change of the user's own, or
     *  updating Desktop, fixes it when claude's folder is already on PATH behind it. */
    shadow: !!r.claude_shadowed_by,
  };
}

const POLL_MS = 3000;
const INSTALL_TIMEOUT_MS = 20 * 60 * 1000;

function createClaudeSetupStore() {
  let open = $state(false);
  let report = $state<ClaudeSetupReport | null>(null);
  let checking = $state(false);
  let error = $state<string | null>(null);
  /** The install running in a tab, and whether to apply the remaining fixes when it lands. */
  let install = $state<{ kind: InstallKind; tabId: string; tabName: string; applyAfter: boolean; started: number } | null>(null);
  /** An install tab is being opened: `install` is set only once its command is typed, seconds
   *  later, and a second click in between would start a second installer. */
  let starting = $state(false);
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  /** A probe can take seconds (`claude --version`, `wsl -l -v`) and they overlap: only the
   *  newest one started may write `report`, or a slow older one puts back what was missing. */
  let generation = 0;

  async function refresh(): Promise<ClaudeSetupReport | null> {
    const mine = ++generation;
    checking = true;
    try {
      const r = await commands.detectClaudeSetup();
      if (mine === generation) {
        report = r;
        // What a fresh look shows replaces what an earlier step said went wrong.
        error = null;
      }
      return r;
    } catch (e) {
      if (mine === generation) error = `Couldn't check this computer: ${e}`;
      return null;
    } finally {
      if (mine === generation) checking = false;
    }
  }

  /** A new tab in the active pane, with `command` typed once its shell is up. Enter is `\r`:
   *  that is what the Enter key sends, and PowerShell under ConPTY doesn't take `\n` as one. */
  async function runInNewTab(name: string, command: string): Promise<string | null> {
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    if (!ws || !pane) {
      error = 'Open a workspace first: the install runs in a tab.';
      return null;
    }
    const tab = await workspacesStore.createTab(ws.id, pane.id, name);
    for (let i = 0; i < 100 && !terminalsStore.get(tab.id); i++) {
      await new Promise(r => setTimeout(r, 100));
    }
    const inst = terminalsStore.get(tab.id);
    if (!inst) {
      error = `The "${name}" tab didn't start. Run this in any tab instead:\n${command}`;
      return null;
    }
    // Give the shell a moment to draw its prompt; anything typed earlier is type-ahead, which
    // every Windows shell accepts, but a prompt first reads better in the tab.
    await new Promise(r => setTimeout(r, 800));
    await commands.writeTerminal(inst.ptyId, Array.from(new TextEncoder().encode(command + '\r')));
    return tab.id;
  }

  function stopPolling() {
    if (pollTimer) clearTimeout(pollTimer);
    pollTimer = null;
  }

  function tabStillExists(tabId: string): boolean {
    return workspacesStore.workspaces.some(ws => ws.panes.some(p => p.tabs.some(t => t.id === tabId)));
  }

  /** Watch for the install to land: detection, not the tab's output, decides — an installer's
   *  wording is not ours to parse, and the file appearing is the fact that matters. */
  function poll() {
    stopPolling();
    pollTimer = setTimeout(async () => {
      const job = install;
      if (!job) return;
      const r = await refresh();
      const m = r ? missing(r) : null;
      const landed = !!m && (job.kind === 'claude' ? !m.claude : job.kind === 'git' ? !m.git : !m.git && !m.claude);
      if (landed) {
        install = null;
        if (job.applyAfter && r) await applyFixes(r);
        open = true; // show the result, wherever the user is
        return;
      }
      if (!tabStillExists(job.tabId) || Date.now() - job.started > INSTALL_TIMEOUT_MS) {
        install = null;
        error = `The install in "${job.tabName}" hasn't finished. Check that tab, then press Check again.`;
        return;
      }
      poll();
    }, POLL_MS);
  }

  /** The fixes that need no download: Git Bash for new tabs, and claude's folder on PATH —
   *  decided from the probe the caller just made, not whatever `report` holds by now. */
  async function applyFixes(r: ClaudeSetupReport) {
    const m = missing(r);
    let failed: string | null = null;
    try {
      if (m.shell) await preferencesStore.setWindowsShell('auto');
      if (m.path) await commands.claudeSetupAddToPath();
    } catch (e) {
      failed = String(e);
      logError(`Claude setup fix failed: ${e}`);
    }
    await refresh();
    if (failed) error = failed; // after the re-check, which clears errors
  }

  return {
    get open() { return open; },
    get report() { return report; },
    get checking() { return checking; },
    get error() { return error; },
    get install() { return install; },
    /** An install is running or its tab is being opened: every install button waits. */
    get busy() { return !!install || starting; },

    show() {
      open = true;
      error = null;
      void refresh();
    },
    hide() { open = false; },
    refresh,

    /** First launch on Windows: open by itself once, if anything is missing. */
    async maybeAutoOpen() {
      await preferencesStore.ready;
      if (preferencesStore.claudeSetupPrompted) return;
      const r = await refresh();
      if (!r?.windows) return;
      await preferencesStore.setClaudeSetupPrompted(true);
      const m = missing(r);
      if (m.git || m.claude) open = true;
    },

    /** Install in a visible tab, then watch for it. `applyAfter` = Fix everything. */
    async runInstall(kind: InstallKind, applyAfter = false) {
      if (install || starting) return;
      starting = true;
      error = null;
      try {
        const tabName = kind === 'git' ? 'Install Git' : kind === 'claude' ? 'Install Claude' : 'Install Git + Claude';
        const tabId = await runInNewTab(tabName, installCommand(kind));
        if (!tabId) return;
        install = { kind, tabId, tabName, applyAfter, started: Date.now() };
        poll();
      } finally {
        starting = false;
      }
    },

    /** Everything at once: install whatever is missing (Git only through winget), then the fixes. */
    async fixEverything() {
      if (install || starting) return;
      const r = await refresh();
      if (!r) return;
      const m = missing(r);
      const git = m.git && r.winget;
      if (git || m.claude) {
        await this.runInstall(git && m.claude ? 'both' : git ? 'git' : 'claude', true);
      } else {
        await applyFixes(r);
      }
    },

    async useGitBash() {
      await preferencesStore.setWindowsShell('auto');
      await refresh();
    },

    async addToPath() {
      let failed: string | null = null;
      try {
        await commands.claudeSetupAddToPath();
      } catch (e) {
        failed = String(e);
      }
      await refresh();
      if (failed) error = failed; // after the re-check, which clears errors
    },

    /** The real test: a new tab (with the shell new tabs now get) running `claude`. */
    async startClaude() {
      if (await runInNewTab('Claude', 'claude')) open = false;
    },

    showInstallTab() {
      const job = install;
      if (!job) return;
      open = false;
      void navigateToTab(job.tabId);
    },
  };
}

export const claudeSetupStore = createClaudeSetupStore();
