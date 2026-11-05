/**
 * The window's half of a media install (docs/hive-media.md §6, Rust `hive/media/install.rs`).
 *
 * `media.install.start {install_id, provider_id, title, line}` arrives over `hive-frontend-request`
 * after the human's click on `hiveMediaInstall`. Rust built `line` from the catalogue; this opens a
 * visible terminal tab on this machine, waits for its shell at a settled prompt (the stack's guard),
 * arms the install in Rust, types the line and answers `{tab_id, pty_id, watches_exit, began}`.
 * Rust reads the exit itself from OSC 133 and emits `hive-media-install-finished`.
 *
 * The tab goes in a workspace that takes a terminal tab: the active one unless it is a Hive (whose
 * workspace takes no new tabs), the Overlord's, suspended or the `avoid_workspace_id` the sheet
 * passed (the Create sheet's, about to become a Hive), else the first such workspace in this
 * window, else a new "Setup" one. The tab becomes its pane's active tab; the window's view isn't
 * switched (the setup sheet may `navigateToTab(tab_id)`).
 */
import * as commands from '$lib/tauri/commands';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { stackStore } from '$lib/stores/stack.svelte';
import type { Workspace } from '$lib/tauri/types';
import { info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log';

const MOUNT_WAIT_MS = 15000;

/** Whether a new terminal tab may open in `ws`. */
export function takesInstallTab(ws: Workspace): boolean {
  return !ws.hive && !ws.overlord && !ws.suspended && ws.panes.length > 0;
}

/** The workspace an install's tab opens in, by the module's rule, or null when none takes it.
 *  `avoid` is never picked: the Create sheet's workspace, about to become a Hive (it isn't one yet,
 *  so `takesInstallTab` alone would take it). */
export function installWorkspace(workspaces: Workspace[], activeId: string | null, avoid: string | null = null): Workspace | null {
  const takes = (w: Workspace) => w.id !== avoid && takesInstallTab(w);
  const active = workspaces.find((w) => w.id === activeId);
  if (active && takes(active)) return active;
  return workspaces.find(takes) ?? null;
}

async function targetWorkspace(avoid: string | null): Promise<Workspace> {
  const found = installWorkspace(workspacesStore.workspaces, workspacesStore.activeWorkspaceId, avoid);
  if (found) return found;
  // Every workspace here is a Hive, the Overlord's or suspended: a new one, without moving the human.
  const previous = workspacesStore.activeWorkspaceId;
  const ws = await workspacesStore.createWorkspace('Setup');
  if (previous) await workspacesStore.setActiveWorkspace(previous).catch(() => {});
  return workspacesStore.workspaces.find((w) => w.id === ws.id) ?? ws;
}

export async function startMediaInstall(a: Record<string, unknown>): Promise<unknown> {
  const { install_id: installId, provider_id: providerId, title, line, avoid_workspace_id: avoidRaw } = a;
  if (typeof installId !== 'string' || typeof providerId !== 'string' || typeof title !== 'string' || typeof line !== 'string' || !line || line.includes('\n')) {
    return { error: 'install_id, provider_id, title and a one-line command are required' };
  }
  const avoid = typeof avoidRaw === 'string' && avoidRaw ? avoidRaw : null;
  const ws = await targetWorkspace(avoid);
  const paneId = ws.active_pane_id ?? ws.panes[0]?.id;
  if (!paneId) return { error: 'The workspace for the install has no pane.' };
  // A known place, never inherited: the active tab may be an ssh session, and this runs HERE.
  const tab = await workspacesStore.createTab(ws.id, paneId, title, { context: { cwd: null, sshCommand: null, remoteCwd: null } });
  window.dispatchEvent(new CustomEvent('activate-tab', { detail: tab.id }));
  // Nothing typed: the tab goes, so a refused install leaves nothing behind.
  const refuse = async (error: string) => {
    logWarn(`media install ${providerId}: not started: ${error}`);
    await workspacesStore.deleteTab(ws.id, paneId, tab.id).catch((e) => logWarn(`media install: close tab ${tab.id}: ${e}`));
    return { error };
  };
  await terminalsStore.waitForRegister(tab.id, MOUNT_WAIT_MS);
  const instance = terminalsStore.get(tab.id);
  if (!instance) return refuse('its terminal tab could not be opened');
  const ptyId = instance.ptyId;
  const out = await stackStore.typeAtPrompt(ptyId, line, () => commands.hiveMediaInstallArm(installId, tab.id, ptyId));
  if ('error' in out) return refuse(out.error);
  logInfo(`media install ${providerId}: typed in tab ${tab.id} (OSC 133: ${out.integration}, began: ${out.began})`);
  return { accepted: true, tab_id: tab.id, pty_id: ptyId, watches_exit: out.integration, began: out.began };
}
