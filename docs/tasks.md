# maiTerm Tasks — a first-class project & task system

> Status: **planned**. Date: 2026-08-23. Owner: Darryl.
> Scope: maiTerm owns task state for every agent tab, across every runtime, instead of
> reading each runtime's private task store. Overlord becomes one consumer of it.

## 1. Why stop piggybacking

Overlord's board was fed by mirroring Claude Code's own task state. That broke three
times in two days, and every failure was **silent**:

| What broke | How it surfaced |
|---|---|
| 256 KB transcript tail truncated lists | found by reasoning about the window, not by symptom |
| `~/.claude/todos/` (TodoWrite) went dead — no non-empty write since February; live store moved to `~/.claude/tasks/` (TaskCreate) | found only after reading a store nobody writes to |
| Files are **deleted** once every task completes, so "finished everything" looks identical to "never tracked" | found only because the operator remembered |

The third one was actively harmful: the board froze finished rows (which then aged into
false "stale" cards) and Overlord would have nudged an agent to *start* a task list at the
exact moment it completed one.

**And it can never be uniform.** Verified 2026-08-23:

| Runtime | Task state available to us |
|---|---|
| Claude | `~/.claude/tasks/<session>/<n>.json` — undocumented, moved once already, sweeps on completion |
| Codex | none. `update_plan` exists only inside rollout JSONL → transcript scraping, different format |
| Gemini | none at all |

So the mirror is a Claude-only feature resting on an undocumented format that changes with
the vendor's release cycle — for a subsystem whose whole promise is "don't lose track of
work". It is also **read-only**: the human can't edit an agent's list from the board, and
Overlord can't seed a task. That is a hard ceiling.

> **Decision:** maiTerm owns the store. Every runtime gets the same contract. The Claude
> store is demoted from source-of-truth to a one-way **importer**.

## 2. Shape

```
Workspace.tasks[]          ← source of truth (a workspace IS a project)
  ├─ MCP: listTasks / createTasks / updateTasks   ← agents, every runtime, incl. SSH
  ├─ Side panel (per tab + workspace backlog)     ← humans
  ├─ Overlord board                               ← one consumer, not the owner
  └─ Claude task store                            ← importer only, never written back
```

### Why MCP and not a JSON file the agent reads directly

A file is faster per call and needs no tool tokens. It still loses, decisively:

- **SSH tabs put the file on the wrong host.** Remote agents are first-class in maiTerm and
  there is already a reverse-tunnel MCP bridge built so they can call local tools. A
  file contract would mean mirroring JSON over that tunnel and reconciling writes from both
  ends — reinventing the fragile machinery this whole change exists to escape.
- **Latency is a non-issue at this granularity.** An MCP round trip is milliseconds against
  a model turn measured in seconds, and agents already call maiterm tools constantly. "MCP
  is slow" is true for chatty per-token work, false for task mutations.
- One writer, validated payloads, atomic updates, and instant UI refresh with no file
  watcher.

Cost accepted: ~3 extra tool schemas in every agent's context. Mitigated by keeping the
surface to three batched tools.

### Keep the importer

The mirror's superpower is that it needs **zero agent cooperation** — a Claude tab that
never learns our tools still populates the board. That signal is free; only the
*dependency* on it was harmful. It becomes a one-way import marked `origin: 'imported'`,
never written back, and never required for correctness.

## 3. Data model

```rust
// src-tauri/src/state/workspace.rs — on Workspace, replacing WindowData.overlord_tasks
pub struct Task {
    pub id: String,
    pub title: String,
    /// Longer body: acceptance criteria, links, notes. Markdown, human- and agent-editable.
    pub detail: Option<String>,
    /// "backlog" | "active" | "blocked" | "review" | "done"
    pub status: String,
    /// Assignee tab; None = workspace backlog, unassigned.
    pub tab_id: Option<String>,
    /// Task ids that must finish first. A non-done task with unmet deps renders as blocked.
    pub blocked_by: Vec<String>,
    /// "human" | "agent" | "overlord" | "imported"
    pub origin: String,
    pub created_at: String,
    pub updated_at: String,
    /// Mesh topic that is this task's conversation vehicle, if any.
    pub topic_id: Option<String>,
}
```

Ordering is the `Vec` order — no separate rank field; reordering rewrites the vector.

**Storage moves from `WindowData.overlord_tasks` to `Workspace.tasks`** because a workspace
is a project: tasks then survive window moves, travel with a duplicated/exported workspace,
and are naturally scoped for the panel. A one-time migration reassigns existing rows by
their `workspace_id`.

**Status vocabulary is unchanged** (`backlog/active/blocked/review/done`) so the Overlord
board's five lanes and every existing helper keep working. Imported/agent statuses map:
`pending → backlog`, `in_progress → active`, `completed → done`, plus unmet `blocked_by`
→ `blocked`.

## 4. MCP surface

Three tools, batched to keep both token cost and round trips down. Registered for every
runtime; they ride the SSH bridge like every other maiterm tool.

```ts
listTasks({ scope?: 'tab' | 'workspace' })   // default 'workspace'
  → { tasks: [{ id, title, detail?, status, tab_id, blocked_by, origin, updated_at }] }

createTasks({ tasks: [{ title, detail?, status?, blocked_by?, assign_to_me? }] })
  → { created: string[] }                    // ids, in order

updateTasks({ updates: [{ id, status?, title?, detail?, blocked_by? }] })
  → { updated: string[], missing: string[] }
```

- All calls are scoped to the **calling tab's workspace** — a tab cannot read or write
  another project's tasks. Identity comes from the connection→tab affinity that
  `initSession` establishes, same as every other tab-scoped tool.
- `assign_to_me` defaults true on create, so an agent's own tasks land on its lane.
- Deletion is deliberately **not** exposed. An agent may mark `done`; only the human
  deletes. Cheap insurance against an agent tidying away work it didn't understand.

### Priming (decided 2026-08-23: every agent tab)

`initSession` already appends an Overlord standing instruction when the window has an
Overlord workspace. It gains a task instruction on **every** agent tab, so task state is
consistent whether or not anyone is supervising:

> "Track multi-step work with the maiTerm task tools (`createTasks`/`updateTasks`) rather
> than your runtime's own todo list, so your human and this window's board can see it.
> Keep statuses current as you go.
>
> **If you already have a task or todo list for this project, migrate it now**: call
> `createTasks` once with the outstanding items (carry their current status across; skip
> anything already finished), then keep working from the maiTerm list."

The migration clause matters because `initSession` fires on **resume, fork and compact**,
not just on a fresh session — which is exactly when an agent is mid-project with a live
list. Without it, adoption waits for the agent's next multi-step task and everything
already in flight stays invisible to the human and the board. With it, a tab that resumes
into a half-finished plan publishes that plan on its first turn.

Three constraints on that clause:

- **Once, not every turn.** Re-priming on each compact must not re-create the same rows.
  The agent is told to migrate *outstanding* items, and the importer's dedup (§7.3,
  normalized title within a tab) is the mechanical backstop — the instruction alone is not
  relied on for correctness.
- **It overlaps the importer, deliberately.** For a Claude tab both paths can fire: the
  agent ports its list, and the importer mirrors the same store. They converge on the same
  dedup key, so the overlap is redundancy rather than duplication — and it is the only
  route at all for Codex and Gemini, which have no store to import from.
- **Finished work is not migrated.** Items already `completed` stay behind; porting them
  would fill a fresh board with history nobody asked for.

Gated on a `tasks_enabled` preference (default on) so it can be switched off wholesale.

## 5. Side panel

Mirrors the notes panel exactly — that pattern is proven and the muscle memory transfers.

- Component `src/lib/components/tasks/TasksPanel.svelte`, rendered in
  `SplitPane.svelte:183` beside `NotesPanel` (same `{#key activeTab.id}` guard).
- Visibility via `workspacesStore.toggleTasks(tabId)` / `isTasksVisible(tabId)`, copying
  `toggleNotes`/`isNotesVisible` (`workspaces.svelte.ts:2297`, `:2320`).
- Width persisted as a `tasks_width` preference, drag-resized from the left edge like
  `NotesPanel.svelte:198`.
- Keyboard: **Cmd+Shift+E** (Cmd+E is notes).
- Content: this tab's tasks first, then the workspace backlog; inline add, click-to-edit
  title/detail, status cycling, assign/unassign, and a blocked-by indicator.

## 6. Build order

1. **Model + storage.** `Task` on `Workspace`, migration off `WindowData.overlord_tasks`,
   Tauri commands, TS mirrors.
2. **Store.** `tasks.svelte.ts` — per-window view across workspaces, CRUD, reactive.
   Overlord board reads from it; its own list is deleted.
3. **Importer.** The Claude task-store mirror writes into it as `origin: 'imported'`.
4. **MCP.** Three tools + frontend handlers + initSession priming.
5. **Panel.** `TasksPanel.svelte`, visibility, preference, shortcut.
6. **Overlord follow-up.** `task_stale` and the ask-to-track nudge retarget to maiTerm
   tasks; the scan's "untracked" bucket means "no maiTerm tasks" rather than "no todos".

Stages 1–2 are a refactor with no user-visible change; 3–5 are the feature.

## 7. Open questions

1. **Cross-workspace view.** The Overlord board already groups by workspace, so per-window
   aggregation is free. A global "everything, everywhere" view is deferred.
2. **Conflict handling.** Two agents in one workspace updating the same task is possible
   but rare; last-write-wins for v1, with `updated_at` making it visible.
3. **Importer collisions.** A Claude tab that also uses `createTasks` could double-record
   one piece of work. Dedup on normalized title within a tab, same approach the TodoWrite
   mirror already uses.

## 8. Rejected

| Rejected | Why |
|---|---|
| Keep mirroring each runtime's private store | Three formats, one undocumented and moving, one scrape-only, one absent. Read-only ceiling. Three silent failures in two days. |
| JSON file the agent reads/writes directly | Wrong host for SSH tabs; needs write reconciliation over the tunnel; no validation or atomicity. Speed advantage is irrelevant next to a model turn. |
| Tasks on `WindowData` (status quo) | A workspace is the project. Window-scoped tasks don't survive moves and don't travel with an exported workspace. |
| Exposing task deletion over MCP | An agent tidying away work it didn't understand is unrecoverable. `done` is enough. |
| Per-tab task storage | Tabs are ephemeral (reload mints a new id); projects are not. Tabs are an *assignee*, not an owner. |
