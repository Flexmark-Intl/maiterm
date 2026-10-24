/** What a Hive tool needs done in this window (docs/hive.md §7.2): Rust validates and reserves,
 *  then asks the window that owns the task list (the tasks store writes it whole) and mounts the
 *  tab, over its own event, `hive-frontend-request` (hive/rpc.rs).
 *
 *  Deliberately its OWN dispatcher, not a case in the `claude-code-tool` switch: `tools/call`
 *  doesn't validate tool names, so a verb there would be any agent's to call over MCP.
 *
 *  Every branch MUST answer, a thrown error included: an unanswered spawn waits out its timeout
 *  and is rolled back, after this window may already have adopted half of it. */

import * as commands from '$lib/tauri/commands';
import { tasksStore } from '$lib/stores/tasks.svelte';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import type { Tab, Task, Workstream } from '$lib/tauri/types';
import { error as logError } from '@tauri-apps/plugin-log';

export interface HiveRequest {
  request_id: string;
  verb: string;
  args: Record<string, unknown>;
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/** `hive.spawn {workspace_id, task, workstream, tab?}`: the worker's row, then (a launched
 *  worker) its hidden tab, mounted so its terminal types the launch builder's line. A waiting
 *  worker comes without a tab. */
function spawn(a: Record<string, unknown>): unknown {
  const { workspace_id: workspaceId, task, workstream, tab } = a;
  if (typeof workspaceId !== 'string' || !isObject(task) || typeof task.id !== 'string') {
    return { error: 'workspace_id and task are required' };
  }
  if (!workspacesStore.workspaces.find((w) => w.id === workspaceId)?.hive) return { error: 'This window holds no such Hive.' };
  if (tab !== undefined && tab !== null && (!isObject(tab) || typeof tab.id !== 'string')) return { error: 'tab must be a tab' };
  // The tab first: it is the half that can refuse, and a row adopted before a refusal would
  // stay until the unspawn after it.
  if (isObject(tab)) workspacesStore.adoptHiveTab(workspaceId, tab as unknown as Tab);
  tasksStore.adoptHiveTask(workspaceId, task as unknown as Task, isObject(workstream) ? (workstream as unknown as Workstream) : null);
  // A hidden tab is never a pane's active tab, so only this mounts it (as for the coordinator).
  if (isObject(tab)) window.dispatchEvent(new CustomEvent('activate-tab', { detail: tab.id }));
  return { ok: true };
}

/** `hive.unspawn {workspace_id, task_id, tab_id?}`: a spawn Rust rolled back. Whatever of it
 *  this window took goes; what it never took is no error. */
function unspawn(a: Record<string, unknown>): unknown {
  const { workspace_id: workspaceId, task_id: taskId, tab_id: tabId } = a;
  if (typeof workspaceId !== 'string' || typeof taskId !== 'string') return { error: 'workspace_id and task_id are required' };
  if (typeof tabId === 'string') workspacesStore.dropHiveTab(workspaceId, tabId);
  if (tasksStore.find(workspaceId, taskId)) tasksStore.remove(workspaceId, taskId);
  return { ok: true };
}

export async function handleHiveRequest(req: HiveRequest): Promise<void> {
  const { request_id, verb } = req;
  const args = isObject(req.args) ? req.args : {};
  let result: unknown;
  try {
    switch (verb) {
      case 'hive.spawn':
        result = spawn(args);
        break;
      case 'hive.unspawn':
        result = unspawn(args);
        break;
      default:
        result = { error: `Unknown Hive request: ${verb}` };
    }
  } catch (err) {
    logError(`hive request ${verb} failed: ${err}`);
    result = { error: String(err instanceof Error ? err.message : err) };
  }
  try {
    await commands.claudeCodeRespond(request_id, result);
  } catch (err) {
    logError(`hive request ${verb}: respond failed: ${err}`);
  }
}
