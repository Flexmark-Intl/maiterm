/**
 * Put file paths onto a terminal tab's command line: insert, never submit
 * (docs/file-tree.md §9.5).
 *
 * The one path for everything that hands files to a tab: a file dropped from the OS onto a
 * terminal, and (with Files) "Add to ‹tab›". Nothing here ever types a CR — the human reads
 * what landed and presses Enter themselves.
 *
 * Rules, in order:
 * 1. Only a live, visible-in-the-strip tab: a stack service's shell is maiTerm's, not a place
 *    to type into.
 * 2. Never while the tab shows a dialog (hooked, on screen, or Claude's trust dialog). A typed
 *    key picks a row there, and the space after a path is a keypress; Enter on the trust dialog
 *    confirms "No, exit". Refused with a toast, nothing typed.
 * 3. An agent tab gets each absolute path plus a space, 200 ms apart, so the agent's input box
 *    sees each as its own file reference.
 * 4. A plain shell gets the paths quoted for its shell (`shellQuote`), space-separated.
 * 5. An ssh tab: a path already on that host is typed as is; a local file is uploaded first
 *    (an agent's into `AGENT_UPLOAD_DIR`, a shell's into its working folder).
 */
import { agentDialogOpen, getPtyInfo, writeTerminal } from '$lib/tauri/commands';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { getBridgeInfo } from '$lib/stores/sshMcpBridge.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { uploadWithProgress, AGENT_UPLOAD_DIR } from '$lib/utils/scpUpload';
import { shellQuote, localShellKind } from '$lib/utils/shellQuote';
import { isWindows } from '$lib/utils/platform';
import { basenameOf } from '$lib/utils/pathForm';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

export interface InsertItem {
  /** Absolute local path. For a remote item, its display form. */
  abs: string;
  /** Set when the file lives on an ssh host: that host's tunnel key and the path there. */
  remote?: { hostKey: string; path: string };
}

export interface InsertOptions {
  /** The tab's ssh foreground command, when the caller already probed it (the drop path
   *  probes on drag-enter). `undefined` probes here; `null` means "not ssh". */
  sshCommand?: string | null;
}

export type InsertResult = { ok: true } | { ok: false; reason: string };

/** Gap between paths typed into an agent: one write per file reference. */
const AGENT_PATH_GAP_MS = 200;

const encode = (s: string) => Array.from(new TextEncoder().encode(s));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const plural = (n: number) => `${n} file${n > 1 ? 's' : ''}`;

function refuse(reason: string, toastTitle = 'Paths not inserted'): InsertResult {
  toastStore.addToast(toastTitle, reason, 'info');
  return { ok: false, reason };
}

/** A dialog in the tab, read fresh. A failed read counts as open: typing blind is the risk. */
async function dialogOpen(tabId: string): Promise<boolean> {
  return agentDialogOpen(tabId).catch(() => true);
}

const DIALOG_REASON = 'The agent is showing a dialog, and a typed key would answer it. Answer it first, then try again.';

/** Type each path plus a space into an agent tab, asking about a dialog before every write:
 *  one can open between two paths. */
async function typeIntoAgent(tabId: string, ptyId: string, paths: string[]): Promise<InsertResult> {
  for (let i = 0; i < paths.length; i++) {
    if (i > 0) await sleep(AGENT_PATH_GAP_MS);
    if (await dialogOpen(tabId)) return refuse(DIALOG_REASON);
    await writeTerminal(ptyId, encode(paths[i] + ' '), true);
  }
  return { ok: true };
}

export async function insertPathsIntoTab(
  tabId: string,
  items: InsertItem[],
  opts: InsertOptions = {},
): Promise<InsertResult> {
  if (items.length === 0) return { ok: false, reason: 'nothing to insert' };

  // 1. Live, and not a stack service.
  const inst = terminalsStore.get(tabId);
  if (!inst) return refuse('That tab has no running terminal.');
  if (workspacesStore._locateTab(tabId)?.tab.service_id) return refuse('That tab runs a stack service.');
  const ptyId = inst.ptyId;

  // 2. No dialog on screen. Asked again right before anything is typed, since an upload can
  //    take long enough for one to appear.
  if (await dialogOpen(tabId)) return refuse(DIALOG_REASON);

  let sshCommand = opts.sshCommand;
  if (sshCommand === undefined) {
    sshCommand = await getPtyInfo(ptyId)
      .then((i) => i.foreground_command ?? null)
      .catch((e) => { logError(`insertPaths: getPtyInfo failed: ${e}`); return null; });
  }
  const isAgent = !!claudeStateStore.getState(tabId);

  const remoteItems = items.filter((i) => i.remote);
  const localPaths = items.filter((i) => !i.remote).map((i) => i.abs);

  // 5a. Paths already on a host go only to a tab on that same host.
  if (remoteItems.length > 0) {
    const tabHost = sshCommand ? getBridgeInfo(tabId)?.hostKey ?? null : null;
    if (!tabHost || remoteItems.some((i) => i.remote!.hostKey !== tabHost)) {
      return refuse('Those files are on another machine than this tab.');
    }
    if (localPaths.length > 0) return refuse('Local and remote files can’t be added together.');
    const paths = remoteItems.map((i) => i.remote!.path);
    if (isAgent) return typeIntoAgent(tabId, ptyId, paths);
    if (await dialogOpen(tabId)) return refuse(DIALOG_REASON);
    await writeTerminal(ptyId, encode(paths.map((p) => shellQuote(p, 'posix')).join(' ')), true);
    return { ok: true };
  }

  // 5b. Local files into an ssh tab: upload, then type where they landed.
  if (sshCommand) {
    let remoteDir = AGENT_UPLOAD_DIR;
    if (!isAgent) {
      const osc = terminalsStore.getOsc(tabId);
      const osc7Cwd = osc?.cwd ?? null;
      const promptCwd = osc?.promptCwd ?? null;
      remoteDir = (osc7Cwd ?? promptCwd ?? '~').trim();
      logInfo(`insertPaths: remoteCwd=${remoteDir} (osc7=${osc7Cwd}, prompt=${promptCwd})`);
    }
    const count = localPaths.length;
    logInfo(`insertPaths SSH: uploading ${count} file(s) to ${remoteDir} via ${sshCommand} (agent=${isAgent})`);
    const outcome = await uploadWithProgress(sshCommand, localPaths, remoteDir);
    if (outcome.status === 'error') {
      logError(`insertPaths SCP upload failed: ${outcome.error}`);
      toastStore.addToast('SCP Upload Failed', outcome.error ?? 'Upload failed', 'error');
      return { ok: false, reason: outcome.error ?? 'upload failed' };
    }
    if (outcome.status === 'cancelled') return { ok: false, reason: 'upload cancelled' };

    // Named as scp names the uploaded file: a backslash is part of a macOS/Linux file name.
    const basenames = localPaths.map((p) => basenameOf(p, isWindows()));
    if (isAgent) {
      const r = await typeIntoAgent(tabId, ptyId, basenames.map((b) => `${AGENT_UPLOAD_DIR}/${b}`));
      if (r.ok) toastStore.addToast('SCP Upload', `${plural(count)} uploaded`, 'success');
      return r;
    }
    // A shell: nothing is typed (its line may hold a half-written command); the toast offers
    // to list what arrived, which the human starts with a click.
    const lCmd = `l ${basenames.map((b) => shellQuote(b, 'posix')).join(' ')}\n`;
    toastStore.addToast(
      'SCP Upload',
      `${plural(count)} uploaded — click to list`,
      'success',
      undefined,
      undefined,
      () => { writeTerminal(ptyId, encode(lCmd)).catch((e) => logError(String(e))); },
    );
    return { ok: true };
  }

  // 3. A local agent: absolute paths, one write each.
  if (isAgent) {
    logInfo(`insertPaths local agent: sending ${localPaths.length} file path(s)`);
    return typeIntoAgent(tabId, ptyId, localPaths);
  }

  // 4. A local shell: quoted for the shell this tab runs.
  const kind = localShellKind(isWindows(), preferencesStore.windowsShell);
  await writeTerminal(ptyId, encode(localPaths.map((p) => shellQuote(p, kind)).join(' ')), true);
  return { ok: true };
}
