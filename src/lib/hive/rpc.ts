/** What a Hive tool needs done in this window (docs/hive.md §7.2): Rust validates and reserves,
 *  then asks the window that owns the task list (the tasks store writes it whole) and mounts the
 *  tab, over its own event, `hive-frontend-request` (hive/rpc.rs).
 *
 *  Deliberately its OWN dispatcher, not a case in the `claude-code-tool` switch: `tools/call`
 *  doesn't validate tool names, so a verb there would be any agent's to call over MCP.
 *
 *  The run queue's verbs ride the same channel (`stack.job.*`): Rust schedules, the stack store
 *  owns the PTYs (docs/hive.md §6.8).
 *
 *  Every branch MUST answer, a thrown error included: an unanswered spawn waits out its timeout
 *  and is rolled back, after this window may already have adopted half of it. */

import * as commands from '$lib/tauri/commands';
import { tasksStore } from '$lib/stores/tasks.svelte';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { followUpsStore } from '$lib/stores/followUps.svelte';
import { stackStore } from '$lib/stores/stack.svelte';
import { appendNote, TASK_STATUSES } from '$lib/tasks/model';
import type { Service, Tab, Task, TaskBlocker, TaskStatus, Workstream } from '$lib/tauri/types';
import { error as logError } from '@tauri-apps/plugin-log';

export interface HiveRequest {
  request_id: string;
  verb: string;
  args: Record<string, unknown>;
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const strings = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);

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

/** `hive.task {workspace_id, task_id, note?, status?, blocker?, expect_asked_at?}`: a worker's row
 *  patched by this store, which owns the list (docs/hive.md §7.2): a report's note and `review`, a
 *  stop's note, a sandbox refusal's blocker, the coordinator's answer to the worker's question.
 *  Rust checked what it says; the row must be here, and with `expect_asked_at` its question must
 *  still be that one (asked again meanwhile, the answer was for the old one). */
function patchTask(a: Record<string, unknown>): unknown {
  const { workspace_id: workspaceId, task_id: taskId, note, status, blocker, expect_asked_at: expectAskedAt } = a;
  if (typeof workspaceId !== 'string' || typeof taskId !== 'string') return { error: 'workspace_id and task_id are required' };
  const row = tasksStore.find(workspaceId, taskId);
  if (!row) return { error: "This window's board has no such task." };
  if (typeof expectAskedAt === 'string' && row.blocker?.asked_at !== expectAskedAt) {
    return { error: 'stale: the worker has asked something new since, or the question was already answered' };
  }
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

/** `hive.retire {workspace_id, task_id, tab_id?, status, note?}`: a worker left for good (docs/hive.md
 *  §8.4). Its tab leaves the mirror, which unmounts its terminal and kills the PTY; its row moves
 *  to `status` and stays, as history. Rust has done both in its own state. */
function retire(a: Record<string, unknown>): unknown {
  const { workspace_id: workspaceId, task_id: taskId, tab_id: tabId, status, note } = a;
  if (typeof workspaceId !== 'string' || typeof taskId !== 'string') return { error: 'workspace_id and task_id are required' };
  if (typeof status !== 'string' || !STATUSES.includes(status)) return { error: `status ${String(status)} isn't a lane` };
  if (typeof tabId === 'string') workspacesStore.dropHiveTab(workspaceId, tabId);
  const row = tasksStore.find(workspaceId, taskId);
  if (!row) return { ok: true, row: 'gone' };
  const patch: Partial<Task> = { status: status as TaskStatus };
  if (typeof note === 'string' && note.trim()) patch.notes = appendNote(row, note, 'agent');
  tasksStore.update(workspaceId, taskId, patch);
  return { ok: true };
}

/** `hive.delete {workspace_id, tab_ids, job_tab_ids?}`: the Hive deleted (§11.2). Every Hive tab and
 *  run-queue job tab leaves the mirror (each PTY with it) and the rows they owned lose their tab,
 *  the rows kept. The Hive record and the panes' active tabs follow when the delete command
 *  answers (`applyHiveDeleted`). */
function deleteHive(a: Record<string, unknown>): unknown {
  const { workspace_id: workspaceId, tab_ids: tabIds, job_tab_ids: jobTabIds } = a;
  if (typeof workspaceId !== 'string' || !Array.isArray(tabIds)) return { error: 'workspace_id and tab_ids are required' };
  const ids = tabIds.filter((t): t is string => typeof t === 'string');
  for (const t of ids) workspacesStore.dropHiveTab(workspaceId, t);
  for (const t of strings(jobTabIds)) {
    const jobId = workspacesStore.dropJobTab(workspaceId, t);
    if (jobId) stackStore.forgetJob(jobId);
  }
  for (const row of tasksStore.forWorkspace(workspaceId)) {
    if (row.tab_id && ids.includes(row.tab_id)) tasksStore.update(workspaceId, row.id, { tab_id: null });
  }
  return { ok: true };
}

/** `hive.finish {workspace_id, tab_ids}`: the Hive is done (docs/hive.md §11.4). The stack services
 *  it started stop (`stopStack` skips a `shared_from` one, and a stop clears the copy's intent, so a
 *  restart brings none back), and every Hive tab's follow-ups and watch scripts are cancelled.
 *  Best effort: what fails is reported, and Done stands. */
async function finish(a: Record<string, unknown>): Promise<unknown> {
  const { workspace_id: workspaceId, tab_ids: tabIds } = a;
  if (typeof workspaceId !== 'string' || !Array.isArray(tabIds)) return { error: 'workspace_id and tab_ids are required' };
  if (!workspacesStore.workspaces.find((w) => w.id === workspaceId)?.hive) return { error: 'This window holds no such Hive.' };
  const failed: string[] = [];
  // `stopStack` answers each copy that didn't stop rather than throwing.
  let stopped = true;
  try {
    for (const f of await stackStore.stopStack(workspaceId)) {
      stopped = false;
      failed.push(`stack: ${f}`);
    }
  } catch (e) {
    stopped = false;
    failed.push(`stack: ${e}`);
  }
  let cancelled = 0;
  for (const tabId of strings(tabIds)) {
    for (const f of followUpsStore.list(tabId)) {
      if (await followUpsStore.cancel(tabId, f.id)) cancelled++;
      else failed.push(`follow-up ${f.id.slice(0, 8)} on tab ${tabId.slice(0, 8)}`);
    }
  }
  return failed.length ? { ok: false, stopped, cancelled, failed } : { ok: true, stopped, cancelled };
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

/** `stack.job.start {workspace_id, job_id, row, line}`: a queued job's hidden tab, its line typed
 *  through the stack's guard (docs/hive.md §6.8). Answers `{accepted, confirmed, tab_id, pty_id}`
 *  once the shell's B for the line is seen, or `{error}`; Rust reads the exit itself. */
async function jobStart(a: Record<string, unknown>): Promise<unknown> {
  const { workspace_id: workspaceId, job_id: jobId, row, line } = a;
  if (typeof workspaceId !== 'string' || typeof jobId !== 'string' || typeof line !== 'string' || !isObject(row) || row.id !== jobId || row.kind !== 'job') {
    return { error: 'workspace_id, job_id, a job row and a line are required' };
  }
  return stackStore.startJob(workspaceId, jobId, row as unknown as Service, line);
}

/** `stack.job.stop {workspace_id, job_id}`: ^C under the pid guard. A stop that didn't land is
 *  answered `{error}`, so Rust never reads it as done. */
async function jobStop(a: Record<string, unknown>): Promise<unknown> {
  const { workspace_id: workspaceId, job_id: jobId } = a;
  if (typeof workspaceId !== 'string' || typeof jobId !== 'string') return { error: 'workspace_id and job_id are required' };
  const out = await stackStore.stopJob(workspaceId, jobId);
  return 'error' in out ? out : { ok: true, status: out.status };
}

/** `stack.job.cleanup {workspace_id, tab_ids?, job_ids?, force?}`: finished job tabs close;
 *  `force` (a retired worker) closes the ones still running too. */
async function jobCleanup(a: Record<string, unknown>): Promise<unknown> {
  const { workspace_id: workspaceId, tab_ids: tabIds, job_ids: jobIds, force } = a;
  if (typeof workspaceId !== 'string') return { error: 'workspace_id is required' };
  return { ok: true, closed: await stackStore.cleanupJobs(workspaceId, strings(tabIds), strings(jobIds), force === true) };
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
      case 'hive.retire':
        result = retire(args);
        break;
      case 'hive.delete':
        result = deleteHive(args);
        break;
      case 'hive.finish':
        result = await finish(args);
        break;
      case 'stack.job.start':
        result = await jobStart(args);
        break;
      case 'stack.job.stop':
        result = await jobStop(args);
        break;
      case 'stack.job.cleanup':
        result = await jobCleanup(args);
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
