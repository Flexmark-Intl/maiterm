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
import type { Task, TaskStatus, Workstream } from '$lib/tauri/types';
import { findDuplicate, findWorkstream, makeTask, makeWorkstream, normalizeTitle, type TaskInput } from '$lib/tasks/model';

function createTasksStore() {
  let byWorkspace = $state<Map<string, Task[]>>(new Map());
  let streamsByWorkspace = $state<Map<string, Workstream[]>>(new Map());
  let loaded = $state(false);

  /** Persist one workspace's list. Fire-and-forget: the in-memory copy is authoritative
   *  for the UI, and a failed write is logged rather than rolled back (same contract as
   *  the Overlord ledger and mesh topics). */
  function persist(workspaceId: string) {
    const list = ($state.snapshot(byWorkspace.get(workspaceId) ?? []) as Task[]);
    const streams = ($state.snapshot(streamsByWorkspace.get(workspaceId) ?? []) as Workstream[]);
    commands.setWorkspaceTasks(workspaceId, list, streams).catch((e) =>
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

    workstreams(workspaceId: string): Workstream[] {
      return streamsByWorkspace.get(workspaceId) ?? [];
    },

    workstream(workspaceId: string, id: string | null | undefined): Workstream | undefined {
      return id ? this.workstreams(workspaceId).find((w) => w.id === id) : undefined;
    },

    /** Resolve a workstream NAME to its id, creating it if new. Agents pass names, not
     *  ids — requiring a round trip just to record work would make the common case worse.
     *  Deduped on the normalized name so near-duplicate spellings stay one job. */
    ensureWorkstream(workspaceId: string, name: string): Workstream | null {
      const trimmed = name.trim();
      if (!trimmed) return null;
      const existing = findWorkstream(this.workstreams(workspaceId), trimmed);
      if (existing) return existing;
      const created = makeWorkstream(trimmed);
      streamsByWorkspace.set(workspaceId, [...this.workstreams(workspaceId), created]);
      streamsByWorkspace = new Map(streamsByWorkspace);
      return created;
    },

    renameWorkstream(workspaceId: string, id: string, name: string): boolean {
      const list = this.workstreams(workspaceId);
      if (!list.some((w) => w.id === id)) return false;
      streamsByWorkspace.set(
        workspaceId,
        list.map((w) =>
          w.id === id
            ? { ...w, name: name.trim(), normalized_name: normalizeTitle(name), updated_at: new Date().toISOString() }
            : w,
        ),
      );
      streamsByWorkspace = new Map(streamsByWorkspace);
      persist(workspaceId);
      return true;
    },

    /** Tasks in one workstream; pass null for the workspace's loose tasks. */
    inWorkstream(workspaceId: string, workstreamId: string | null): Task[] {
      return this.forWorkspace(workspaceId).filter((t) => (t.workstream_id ?? null) === workstreamId);
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
        const rows = await commands.getWindowTasks();
        byWorkspace = new Map(rows.map(([wsId, list]) => [wsId, list]));
        streamsByWorkspace = new Map(rows.map(([wsId, , streams]) => [wsId, streams]));
        loaded = true;
      } catch (e) {
        logError(`tasks: load failed: ${e}`);
      }
    },

    /** Add one task. Returns the created row, or the existing one when an identical
     *  title is already on the same tab — callers treat creation as idempotent so a
     *  re-primed agent re-sending its list is a no-op rather than a duplication. */
    /** Replace one workspace's copy with what the BACKEND just wrote — a phone edit through
     *  maiLink (`mailink-tasks-changed`). No persist: Rust already saved it, and this store's
     *  whole-list persist would otherwise clobber the phone's row on the next desktop edit,
     *  which is the reason the event exists. Ignored for a workspace this window doesn't hold —
     *  the event is app-wide and persisting a foreign workspace would fail "not found". */
    applyFromBackend(workspaceId: string, tasks: Task[], workstreams: Workstream[]) {
      if (!byWorkspace.has(workspaceId)) return;
      byWorkspace.set(workspaceId, tasks);
      byWorkspace = new Map(byWorkspace);
      streamsByWorkspace.set(workspaceId, workstreams);
      streamsByWorkspace = new Map(streamsByWorkspace);
    },

    add(workspaceId: string, input: TaskInput): Task {
      const list = this.forWorkspace(workspaceId);
      const dup = findDuplicate(list, input.title, input.tab_id, input.workstream_id);
      if (dup) {
        const patch: Partial<Task> = {};
        // Reclaimed from the backlog (the caller's previous tab id died) — take ownership
        // so it shows as this tab's work again rather than sitting unassigned. The caller's
        // status wins: it describes the work as it stands now, where the released row's is
        // a snapshot from before its old tab went away.
        if (!dup.tab_id && input.tab_id) {
          patch.tab_id = input.tab_id;
          patch.status = input.status ?? dup.status;
        }
        // Matched a loose row while naming a job — file it, rather than leaving the same
        // work in two places depending on which call happened to record it.
        if (input.workstream_id && !dup.workstream_id) patch.workstream_id = input.workstream_id;
        if (Object.keys(patch).length) this.update(workspaceId, dup.id, patch);
        return dup;
      }
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
        const dup = findDuplicate(list, input.title, input.tab_id, input.workstream_id);
        if (dup) {
          const claimed = { ...dup };
          let touched = false;
          // Reclaimed from the backlog — take ownership back, honouring the caller's
          // status: it describes the work now, where the released row's is a snapshot
          // from before its old tab went away.
          if (!dup.tab_id && input.tab_id) {
            claimed.tab_id = input.tab_id;
            claimed.status = input.status ?? dup.status;
            touched = true;
          }
          // Matched a loose row while naming a job — file it under that job, rather than
          // leaving the same work in two places depending on which call recorded it.
          if (input.workstream_id && !dup.workstream_id) {
            claimed.workstream_id = input.workstream_id;
            touched = true;
          }
          if (touched) {
            claimed.updated_at = new Date().toISOString();
            list[list.indexOf(dup)] = claimed;
            out.push(claimed);
            added = true;
          } else {
            out.push(dup);
          }
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

    /** Delete. Deliberately human-only — no MCP tool reaches this (docs/tasks.md §5).
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

    /** Reassign a tab's tasks to its replacement, for paths that mint a new id for the
     *  SAME work — reload, and anything else that rebuilds a tab in place.
     *
     *  Preferred over release-and-reclaim whenever the new id is known: it is exact, it
     *  keeps the tasks visible as this tab's throughout, and it cannot be intercepted by
     *  another tab that happens to use the same title. Mirrors agentBridge/agentMesh
     *  `remapTab`, which exist for the same reason. */
    remapTab(oldTabId: string, newTabId: string) {
      for (const [workspaceId, list] of [...byWorkspace]) {
        if (!list.some((t) => t.tab_id === oldTabId)) continue;
        commit(
          workspaceId,
          list.map((t) => (t.tab_id === oldTabId ? { ...t, tab_id: newTabId } : t)),
        );
      }
    },

    /** Release a closing tab's tasks back to the project backlog.
     *
     *  Tab ids are not durable: a reload is duplicate-then-close and a fork mints a new
     *  id, so tasks tagged with the old id would otherwise be stranded on a tab that no
     *  longer exists — invisible under "this tab", permanently unfinished, and passed over
     *  by the dedup when the resumed agent re-sends the same list. Unassigning them makes
     *  them reclaimable (`findDuplicate`) and honest: a task whose assignee is gone belongs
     *  to the project, not to a ghost. Finished rows are left alone; the sweep handles them.
     *
     *  Every workspace is checked, not just the first match: a tab moved between
     *  workspaces leaves tasks behind in the old one, so its rows legitimately span two
     *  lists and stopping early would strand half of them on an id that no longer exists —
     *  unreachable by the panel, by the importer, and by the dedup. */
    /**
     * Mirror a move Rust has ALREADY made and persisted (tab archive / restore).
     *
     * Local only — deliberately no `persist()`. This store writes WHOLE lists, so persisting
     * here would race the very move it is reflecting. And it must run SYNCHRONOUSLY after the
     * command resolves: an awaited `rehydrate()` leaves the mirror stale for a whole IPC
     * round trip, and any writer touching that workspace in the gap computes its whole-list
     * write from the pre-move copy — putting the rows back on the board while the archived
     * tab also holds them (restore then duplicates the ids, which Svelte's keyed `each`
     * throws on), or, on the restore side, dropping the returned rows from both places at
     * once with no copy left anywhere.
     */
    applyTabArchive(workspaceId: string, tabId: string): Task[] {
      const moved = (byWorkspace.get(workspaceId) ?? []).filter((t) => t.tab_id === tabId);
      for (const [wsId, list] of [...byWorkspace]) {
        if (wsId === workspaceId) {
          byWorkspace.set(wsId, list.filter((t) => t.tab_id !== tabId));
          continue;
        }
        // Rows this tab left behind in a workspace it was dragged out of are RELEASED by
        // `archive_tab`, not moved — mirror that here too, or the next whole-list write to
        // that workspace re-attributes them to a tab it no longer has, where the panel shows
        // them as neither `mine` nor `unclaimed` and `findDuplicate` will not reclaim them.
        if (!list.some((t) => t.tab_id === tabId && t.status !== 'done')) continue;
        byWorkspace.set(
          wsId,
          list.map((t) => (t.tab_id === tabId && t.status !== 'done' ? { ...t, tab_id: null } : t)),
        );
      }
      byWorkspace = new Map(byWorkspace);
      return moved;
    },

    /** The other half: rows that came back with a restored tab. */
    applyTabRestore(workspaceId: string, rows: Task[]) {
      if (!rows.length) return;
      const list = byWorkspace.get(workspaceId) ?? [];
      const known = new Set(list.map((t) => t.id));
      byWorkspace.set(workspaceId, [...list, ...rows.filter((t) => !known.has(t.id))]);
      byWorkspace = new Map(byWorkspace);
    },

    releaseTab(tabId: string) {
      // Snapshot the entries: commit() reassigns the Map underneath the iteration.
      for (const [workspaceId, list] of [...byWorkspace]) {
        if (!list.some((t) => t.tab_id === tabId && t.status !== 'done')) continue;
        commit(
          workspaceId,
          list.map((t) => (t.tab_id === tabId && t.status !== 'done' ? { ...t, tab_id: null } : t)),
        );
      }
    },
  };
}

export const tasksStore = createTasksStore();
