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
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { followUpsStore } from '$lib/stores/followUps.svelte';
import { appendNote, TASK_STATUSES } from '$lib/tasks/model';
import type { Tab, Task, TaskBlocker, TaskStatus, Workstream } from '$lib/tauri/types';
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

/** `hive.unspawn {workspace_id, task_id | restore_task, tab_id?}`: a launch Rust rolled back.
 *  Whatever of it this window took goes; what it never took is no error. A worker that existed
 *  before the launch (a waiter, a restart) gets its row back as it was (`restore_task`) instead
 *  of losing it. */
function unspawn(a: Record<string, unknown>): unknown {
  const { workspace_id: workspaceId, task_id: taskId, tab_id: tabId, restore_task: restore } = a;
  const restoring = isObject(restore) && typeof restore.id === 'string';
  if (typeof workspaceId !== 'string' || (typeof taskId !== 'string' && !restoring)) {
    return { error: 'workspace_id and task_id (or restore_task) are required' };
  }
  if (typeof tabId === 'string') workspacesStore.dropHiveTab(workspaceId, tabId);
  if (restoring) tasksStore.adoptHiveTask(workspaceId, restore as unknown as Task, null);
  else if (typeof taskId === 'string' && tasksStore.find(workspaceId, taskId)) tasksStore.remove(workspaceId, taskId);
  return { ok: true };
}

const STATUSES: readonly string[] = TASK_STATUSES;

/** `hive.task {workspace_id, task_id, note?, status?, blocker?}`: a worker's row patched by this
 *  store, which owns the list (docs/hive.md §7.2): a report's note and `review`, a stop's note, a
 *  sandbox refusal's blocker. Rust checked what it says; the row must be here. */
function patchTask(a: Record<string, unknown>): unknown {
  const { workspace_id: workspaceId, task_id: taskId, note, status, blocker } = a;
  if (typeof workspaceId !== 'string' || typeof taskId !== 'string') return { error: 'workspace_id and task_id are required' };
  const row = tasksStore.find(workspaceId, taskId);
  if (!row) return { error: "This window's board has no such task." };
  if (status != null && (typeof status !== 'string' || !STATUSES.includes(status))) return { error: `status ${String(status)} isn't a lane` };
  if (blocker != null && (!isObject(blocker) || typeof blocker.question !== 'string')) return { error: 'blocker must be a blocker' };
  if (blocker != null && status !== 'blocked') return { error: 'a blocker needs status blocked' };
  const patch: Partial<Task> = {};
  if (typeof note === 'string' && note.trim()) patch.notes = appendNote(row, note, 'agent');
  if (typeof status === 'string') patch.status = status as TaskStatus;
  if (isObject(blocker)) patch.blocker = blocker as unknown as TaskBlocker;
  tasksStore.update(workspaceId, taskId, patch);
  return { ok: true };
}

/** `hive.relaunch {workspace_id, tab_id}`: a worker's agent started again in its own tab. A mounted
 *  terminal gets the builder's line through the follow-ups' empty-prompt gate (docs/follow-ups.md
 *  §6.2), never typed over anything; an unmounted tab is mounted, which types it on spawn. */
async function relaunch(a: Record<string, unknown>): Promise<unknown> {
  const { workspace_id: workspaceId, tab_id: tabId } = a;
  if (typeof workspaceId !== 'string' || typeof tabId !== 'string') return { error: 'workspace_id and tab_id are required' };
  const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
  const tab = ws?.panes.flatMap((p) => p.tabs).find((t) => t.id === tabId);
  if (!ws?.hive || tab?.hive_role !== 'worker') return { error: 'This window holds no such worker tab.' };
  if (terminalsStore.get(tabId)) {
    const held = await followUpsStore.relaunchAgent(tabId);
    return held ? { error: held } : { ok: true };
  }
  window.dispatchEvent(new CustomEvent('activate-tab', { detail: tabId }));
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
      case 'hive.task':
        result = patchTask(args);
        break;
      case 'hive.relaunch':
        result = await relaunch(args);
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
