/** Per-window task store (docs/tasks.md).
 *
 *  maiTerm owns task state for every agent tab across every runtime; this store is the
 *  single frontend copy of it. Writers: the side panel (human), the MCP tools (agents,
 *  including SSH tabs), Overlord, and the Claude task-store importer. Everyone goes
 *  through here so there is exactly one in-memory truth and one persistence path.
 *
 *  State is keyed by workspace because that is how it persists — `set_workspace_tasks`
 *  replaces one workspace's whole list, mirroring `setWorkspaceMeshTopics`. Mutations
 *  reassign the Map (Svelte 5 Map reactivity) and persist only the workspace they touched.
 */

import { error as logError } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { Task, TaskStatus } from '$lib/tauri/types';
import { findDuplicate, makeTask, normalizeTitle, type TaskInput } from '$lib/tasks/model';

function createTasksStore() {
  let byWorkspace = $state<Map<string, Task[]>>(new Map());
  let loaded = $state(false);

  /** Persist one workspace's list. Fire-and-forget: the in-memory copy is authoritative
   *  for the UI, and a failed write is logged rather than rolled back (same contract as
   *  the Overlord ledger and mesh topics). */
  function persist(workspaceId: string) {
    const list = ($state.snapshot(byWorkspace.get(workspaceId) ?? []) as Task[]);
    commands.setWorkspaceTasks(workspaceId, list).catch((e) =>
      logError(`tasks: persist failed for workspace ${workspaceId}: ${e}`),
    );
  }

  /** Replace one workspace's list in memory and persist it. */
  function commit(workspaceId: string, list: Task[]) {
    byWorkspace.set(workspaceId, list);
    byWorkspace = new Map(byWorkspace);
    persist(workspaceId);
  }

  return {
    get loaded() { return loaded; },

    /** All workspace ids that currently hold tasks. */
    get workspaceIds(): string[] { return [...byWorkspace.keys()]; },

    forWorkspace(workspaceId: string): Task[] {
      return byWorkspace.get(workspaceId) ?? [];
    },

    forTab(workspaceId: string, tabId: string): Task[] {
      return this.forWorkspace(workspaceId).filter((t) => t.tab_id === tabId);
    },

    /** Workspace backlog: everything in the workspace with no assignee. */
    unassigned(workspaceId: string): Task[] {
      return this.forWorkspace(workspaceId).filter((t) => !t.tab_id);
    },

    find(workspaceId: string, id: string): Task | undefined {
      return this.forWorkspace(workspaceId).find((t) => t.id === id);
    },

    /** Locate a task without knowing its workspace — the MCP handlers know the calling
     *  tab, not necessarily which list an id came from. */
    findAnywhere(id: string): { workspaceId: string; task: Task } | undefined {
      for (const [workspaceId, list] of byWorkspace) {
        const task = list.find((t) => t.id === id);
        if (task) return { workspaceId, task };
      }
      return undefined;
    },

    /** Load every workspace's list for this window. Idempotent; safe to call on remount. */
    async rehydrate() {
      try {
        const pairs = await commands.getWindowTasks();
        byWorkspace = new Map(pairs.map(([wsId, list]) => [wsId, list]));
        loaded = true;
      } catch (e) {
        logError(`tasks: load failed: ${e}`);
      }
    },

    /** Add one task. Returns the created row, or the existing one when an identical
     *  title is already on the same tab — callers treat creation as idempotent so a
     *  re-primed agent re-sending its list is a no-op rather than a duplication. */
    add(workspaceId: string, input: TaskInput): Task {
      const list = this.forWorkspace(workspaceId);
      const dup = findDuplicate(list, input.title, input.tab_id);
      if (dup) return dup;
      const task = makeTask(input);
      commit(workspaceId, [...list, task]);
      return task;
    },

    /** Batch add with dedup applied across the batch as well as against what is already
     *  stored, so one `createTasks` call carrying a repeated title can't self-duplicate. */
    addMany(workspaceId: string, inputs: TaskInput[]): Task[] {
      const list = [...this.forWorkspace(workspaceId)];
      const out: Task[] = [];
      let added = false;
      for (const input of inputs) {
        const dup = findDuplicate(list, input.title, input.tab_id);
        if (dup) {
          out.push(dup);
          continue;
        }
        const task = makeTask(input);
        list.push(task);
        out.push(task);
        added = true;
      }
      if (added) commit(workspaceId, list);
      return out;
    },

    /** Patch a task. `updated_at` is stamped here so every writer gets it for free. */
    update(workspaceId: string, id: string, patch: Partial<Omit<Task, 'id' | 'created_at'>>): boolean {
      const list = this.forWorkspace(workspaceId);
      if (!list.some((t) => t.id === id)) return false;
      commit(
        workspaceId,
        list.map((t) =>
          t.id === id
            ? {
                ...t,
                ...patch,
                normalized_title: patch.title !== undefined ? normalizeTitle(patch.title) : t.normalized_title,
                updated_at: new Date().toISOString(),
              }
            : t,
        ),
      );
      return true;
    },

    setStatus(workspaceId: string, id: string, status: TaskStatus): boolean {
      return this.update(workspaceId, id, { status });
    },

    /** Delete. Deliberately human-only — no MCP tool reaches this (docs/tasks.md §4).
     *  Dangling `blocked_by` edges are cleaned up so no task is left blocked forever by
     *  a prerequisite that no longer exists. */
    remove(workspaceId: string, id: string): boolean {
      const list = this.forWorkspace(workspaceId);
      if (!list.some((t) => t.id === id)) return false;
      commit(
        workspaceId,
        list
          .filter((t) => t.id !== id)
          .map((t) =>
            t.blocked_by?.includes(id) ? { ...t, blocked_by: t.blocked_by.filter((b) => b !== id) } : t,
          ),
      );
      return true;
    },

    /** Whole-list replace for reordering / drag-and-drop, where order IS the change. */
    reorder(workspaceId: string, list: Task[]) {
      commit(workspaceId, list);
    },

    /** Apply a batch of in-place edits as ONE commit. Callers that touch many rows in a
     *  tick (the importer, the engine's sweeps) must use this rather than looping
     *  `update`, which would fire a persist per row. Returns whether anything changed. */
    mutate(workspaceId: string, fn: (list: Task[]) => Task[] | null): boolean {
      const next = fn([...this.forWorkspace(workspaceId)]);
      if (!next) return false;
      commit(workspaceId, next);
      return true;
    },

    /** Drop a workspace's list from memory when the workspace itself goes away. Does not
     *  persist — the workspace record carrying the tasks is already gone. */
    forget(workspaceId: string) {
      if (!byWorkspace.delete(workspaceId)) return;
      byWorkspace = new Map(byWorkspace);
    },
  };
}

export const tasksStore = createTasksStore();
