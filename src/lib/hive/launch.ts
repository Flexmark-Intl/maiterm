/** Every relaunch of an agent tab, a Hive's through its launch builder (docs/hive.md §11.1).
 *
 *  A Hive agent relaunched with the generic `claude --resume %claudeSessionId` comes back without
 *  its folders, unsandboxed, at medium effort and writing into the human's per-repo memory. So a
 *  `hive_role` tab gets the line Rust builds from its launch record, or nothing: a refusal is
 *  surfaced and never falls back to the generic command. */

import { hiveLaunchCommand } from '$lib/tauri/commands';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { interpolateShell } from '$lib/stores/triggers.svelte';
import { resumeCommandFor } from '$lib/stores/agentState.svelte';
import { dispatch } from '$lib/stores/notificationDispatch';
import { error as logError } from '@tauri-apps/plugin-log';

export function isHiveTab(tabId: string): boolean {
  return !!workspacesStore._locateTab(tabId)?.tab.hive_role;
}

/** The Hive launch line for `tabId`, typed raw (never interpolated). Null when Rust refused
 *  ("launch record missing", an old Claude, native Windows…), which the human is told. */
export async function hiveLaunchLine(tabId: string): Promise<string | null> {
  try {
    return await hiveLaunchCommand(tabId);
  } catch (e) {
    logError(`hive: launch refused for tab ${tabId.slice(0, 8)}: ${e}`);
    void dispatch("A Hive agent didn't start", String(e), 'error', { tabId });
    return null;
  }
}

/** The line that relaunches `tabId`'s agent, resuming its own session. An ordinary tab gets its
 *  runtime's resume command with its session id filled in, so a `%` left in it means no session
 *  is known; a Hive tab gets its builder line, or null when that was refused. */
export async function relaunchLine(tabId: string): Promise<string | null> {
  if (isHiveTab(tabId)) return hiveLaunchLine(tabId);
  return interpolateShell(tabId, resumeCommandFor(workspacesStore.getTabRuntime(tabId)));
}
