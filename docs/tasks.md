# maiTerm Tasks — a first-class project & task system

> Status: **implemented** 2026-08-23 (all eight build tasks). Owner: Darryl.
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
    /// Case/whitespace-normalized title — the dedup key within a tab. Recomputed by Rust
    /// on every persist so it can never drift from `title`.
    pub normalized_title: String,
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

**`origin` is load-bearing, not decoration.** The importer only ever *retires* rows it owns
(`imported`); it will not close out or restate work an agent created over MCP (`agent`) or
a human typed (`human`). Without that line the 5s import tick drags a task the agent just
marked done — or the human just moved to review — back to whatever the runtime's stale
file still says. The done-sweep is asymmetric for the same reason: machine rows age out
after 48h, human rows never do.

**Two normalizers, one key.** `Task::normalize_title` (Rust) and `normalizeTitle`
(`src/lib/tasks/model.ts`) must agree exactly, and their language defaults do *not*:
`char::is_whitespace` counts U+0085 NEL but not U+FEFF, and JS `\s` is the mirror image.
Both spell the class out as the union. Matching tests pin it on each side — if they drift,
a pasted title re-duplicates on every restart.

Ordering is the `Vec` order — no separate rank field; reordering rewrites the vector.

**Storage moves from `WindowData.overlord_tasks` to `Workspace.tasks`** because a workspace
is a project: tasks then survive window moves, travel with a duplicated/exported workspace,
and are naturally scoped for the panel. A one-time migration reassigns existing rows by
their `workspace_id`.

### Tab ids are not durable — tasks survive that

A tab id dies more often than the work does: a reload is duplicate-then-close, and a fork
mints a new id too. Both are events `initSession` fires on, so a resumed agent re-sends a
list whose rows all carry an id that no longer exists — and would create a second copy of
everything, once per reload, forever.

Two mechanisms, and the order matters:

- **Remap where the new id is known.** `reloadTab` mints the replacement itself, so it
  calls `tasksStore.remapTab(old, new)` — exact, keeps the tasks visible as the tab's
  throughout, and cannot be intercepted by another tab using the same title. Reload does
  *not* go through the store's `deleteTab`, so nothing else would have moved them.
- **Release and reclaim where it isn't.** A genuine close (`deleteTab`) releases the tab's
  unfinished tasks to the workspace backlog, and `findDuplicate` reclaims an unassigned row
  for a caller that restates its title — via `createTasks`, or via the importer, which must
  take ownership back as well (every completion path is tab-scoped, so a row left
  unassigned can never be closed out). Finished rows stay assigned: a closed-out task
  should not be resurrected and re-owned because a new tab happened to mention it.

`releaseTab` checks every workspace rather than stopping at the first — a tab moved between
workspaces leaves tasks behind, so its rows legitimately span two lists.

This is also the honest model: a task whose assignee is gone belongs to the project, not to
a ghost.

### The six lanes, and what `backlog` actually means

```
BACKLOG   TO-DO   ACTIVE   BLOCKED   REVIEW   DONE
   ^         ^
 parked    new work starts here
```

`backlog` is a **parking lot**, not a to-do list: next month, future ideas, low-priority.
It clears mental clutter, keeps good ideas from being lost, and stops low-priority work
from interrupting what's in flight. `todo` is the not-started-yet lane.

That distinction has teeth, and getting it wrong is what the first version did:

- **New work never starts in `backlog`.** It was the default for every task, which made it
  an inbox while the name promised a parking lot. `makeTask`, `coerceStatus`, and
  `statusFromAgent('pending')` all land in `todo`.
- **Parked tasks are exempt from every "is this in flight" question** — `task_stale`, the
  board's stale signals, `no_todo_list`, and the scan's tracked/untracked classification
  (`isInFlight` / `isParked` in `src/lib/tasks/model.ts`). Before this, `task_stale` fired
  on anything not `done`, so a deliberately shelved item aged into a stale signal and
  Overlord injected a directive about it. **A backlog that generates interruptions is the
  opposite of a backlog.**
- **The done-sweep can't reach them** — it only touches `done` rows, so a shelved idea
  survives indefinitely. A parking lot that quietly empties itself is not one.
- **Backlog is LEFTMOST** even though nothing starts there, because parking something is a
  move *backwards* out of the flow. That is also what makes dragging a card left to shelve
  it read correctly.

Old rows migrate `backlog` → `todo` behind `tasks_backlog_vocabulary_migrated`. The flag is
required: re-running that remap would drag genuinely parked tasks back onto the board.

## 4. Workstreams

One agent tab is routinely asked to do two unrelated things. A **workstream** is a named
job inside a workspace ("Auth refactor", "DB migration"); the workspace is still the
project. Tasks with no workstream are loose — shown, dimmed, never hidden.

```rust
pub struct Workstream { id, name, normalized_name, created_at, updated_at }
// Task.workstream_id: Option<String>   — None = loose
```

Design decisions worth not re-litigating:

- **Not called a "task list."** In kanban a list *is* a column, and there are six of those.
  It also collides with the `listTasks` tool.
- **No `tab_id` on the workstream.** Assignment stays on the `Task`, where the
  release/remap machinery that survives tab-id churn already lives. Every task in a
  workstream shares a tab in practice, so the owner is derivable for display; a second
  source of tab truth would fight that machinery.
- **Agents pass a NAME, not an id.** Requiring a round trip before recording work would
  make the common case worse. `ensureWorkstream` creates-or-reuses, deduped on the same
  normalizer titles use, so one job can't become three spellings.
- **Empty workstreams are dropped on persist.** They exist only to group tasks; one with no
  members is a label with no referent, and agents' throwaway names would accumulate.
- **The dedup key includes the workstream.** "Write the tests" for auth and "write the
  tests" for the DB migration are two different pieces of work.

Imported and agent statuses map: `pending → todo`, `in_progress → active`,
`completed → done`, an explicit `backlog → backlog`, plus unmet `blocked_by` → `blocked`.

## 5. MCP surface

Three tools, batched to keep both token cost and round trips down. Registered for every
runtime; they ride the SSH bridge like every other maiterm tool.

```ts
listTasks({ scope?: 'tab' | 'workspace' })   // default 'workspace'
  → { workspace, scope, workstreams: [{ workstream: string|null, tasks: [...] }] }

createTasks({ workstream?: string,           // a NAME; created if new, reused if not
              tasks: [{ title, detail?, status?, blocked_by?, assign_to_me? }] })
  → { created: string[], workstream?, already_tracked?: string[] }

updateTasks({ updates: [{ id, status?, title?, detail?, workstream?, blocked_by? }] })
  → { updated: string[], missing: string[] }
```

`listTasks` returns tasks **grouped by workstream** rather than flat — a flat list invites
an agent to treat two separate jobs as one, which is the thing workstreams exist to stop.

- All calls are scoped to the **calling tab's workspace** — a tab cannot read or write
  another project's tasks. Identity comes from the connection→tab affinity that
  `initSession` establishes, same as every other tab-scoped tool.
- `assign_to_me` defaults true on create, so an agent's own tasks land on its lane.
- **Statuses are coerced, not trusted.** The MCP layer is hand-rolled JSON-RPC; the
  declared enum is never enforced at runtime, and the priming explicitly asks agents to
  carry their own statuses across. A raw `"completed"` would render in no lane (lanes match
  by equality), never be swept, and read as permanently unfinished to the dependency check —
  wedging everything blocked on it. `coerceStatus` maps it at the handler and Rust clamps
  again before disk.
- Deletion is deliberately **not** exposed. An agent may mark `done`; only the human
  deletes. Cheap insurance against an agent tidying away work it didn't understand.
- But a human deletion has to REACH the agent. `findDuplicate` only sees rows that exist,
  so a task the human removed comes straight back on the agent's next list re-send
  (re-prime, resume, compaction). Both delete paths — the board card and the side panel —
  route through `overlordStore.deleteTask`, which tells the owning tab directly when it is
  idle and hands the notice to the Overlord agent to relay when it isn't. With Overlord
  disabled it is exactly the old silent remove: maiTerm does not type into a terminal on
  behalf of a supervisor that is switched off. See `docs/overlord.md` for why this is a
  notice rather than a directive, and why it is not a tombstone.

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

## 6. Side panel

Mirrors the notes panel exactly — that pattern is proven and the muscle memory transfers.

- Component `src/lib/components/tasks/TasksPanel.svelte`, rendered in
  `SplitPane.svelte:183` beside `NotesPanel` (same `{#key activeTab.id}` guard).
- Visibility via `workspacesStore.toggleTasks(tabId)` / `isTasksVisible(tabId)`, copying
  `toggleNotes`/`isNotesVisible` (`workspaces.svelte.ts:2297`, `:2320`).
- Width persisted as a `tasks_width` preference, drag-resized from the left edge like
  `NotesPanel.svelte:198`.
- Keyboard: **Cmd+Shift+E** (Cmd+E is notes).

**Scope: THIS TAB'S WORK ONLY** (trimmed 2026-08-24). The panel answers one question —
"what am I doing here" — and hands every other question to the board.

Content: this tab's tasks grouped by workstream (headings only when there is more than
one job in view), inline add, click-to-edit title/detail, status cycling, delete, a
blocked-by indicator, and `N parked` / `N done` / `N unclaimed` collapses. One closing
line reports how many tasks are **in flight on other tabs** — a pointer to the board,
never a list, and never a raw row count (that grew with every task the project ever
finished, so it shouted loudest when nothing was happening).

**`N unclaimed` is not a scope violation, it is the only way to reach that work.** The
panel is the sole writer of `Task.tab_id` in the whole frontend — the board reassigns
workstream and status, and `updateTasks` over MCP has no assignee field. So without a
claim control, a row released by `releaseTab` when its tab closed would be unreadable,
uneditable and undeletable from every surface, forever, while still inflating counts.
Unclaimed work is also genuinely this panel's business: it is the pile you can pick up
*here*, not another tab's work. Collapsed by default; ↧ claims, ↥ hands back.

Removed, and why: it also listed **the rest of the project**, offered a **workstream
picker** on every add, and a **per-row workstream dropdown**. All three are *organizing*
work, and organizing has a proper home now — the board
is indexed by workstream (`docs/overlord.md`), where a drag moves a task between jobs and
the whole window is legible at once. Reproducing that in a 280px dock made the panel a
worse board and buried the one list the tab actually needs.

A new task inherits the workstream when every unfinished task on the tab shares one, and
goes loose otherwise — which covers both cases without the picker asking on every add.
`effectiveStatus` still resolves against the whole workspace list, since a prerequisite
can live on another tab; that list is simply never rendered.

## 7. As built

| Stage | Commits |
|---|---|
| Model + storage (`Task` on `Workspace`, migration off `WindowData`, commands, TS mirrors) | `8b4e7ff` |
| Store + cutover (`tasks.svelte.ts`, `model.ts`; Overlord becomes a consumer; `OverlordTask` pruned) | `73b4b23` |
| MCP tools + priming (incl. the migration clause) | `c0b2b3f` |
| Review fixes (eight defects across S1/S2) | `f699d91` |
| Side panel | `39b5545` |
| Overlord signals retargeted | `c499420` |
| Review fixes (five defects across S3) | `50dd2e3` |
| Workstreams + the backlog redefinition | `93c8388` |
| Board: strips, drag/drop, readable descriptions | `0092900` |
| Board re-indexed by workstream (index + one board) | `e585821` |
| Side panel trimmed back to this-tab-only | `00afb21` |

The importer landed with the cutover rather than as its own stage: once Overlord stopped
owning the rows, `syncMirrorTasks` writing `origin: 'imported'` into the store *was* the
importer.

### Migration

`migrate_app_data` drains `WindowData.overlord_tasks` onto the workspace named by each
row's `workspace_id`; the field is deserialize-only raw JSON, so the next save clears it
and the pass cannot run twice. Two details that matter:

- Legacy `origin: "agent"` rows are relabelled **`imported`**. That origin has been
  redefined as MCP-created work, and the importer only retires `imported` — a mislabelled
  mirror could never be closed out and would age into a permanent false "stale" card
  driving `task_stale` rules.
- A row missing any required field is dropped rather than given a fabricated timestamp.
  `updated_at` is exactly what the staleness rules read, so inventing one hands the row a
  bogus age.

## 8. Open questions

1. **Cross-workspace view.** Resolved per-window (`e585821`): the board is indexed by
   workstream across every workspace in the window, and its "Everything" entry is the
   flat aggregate. Workstream is now the navigation axis, so workspace is a grouping
   header in the index rather than a level you walk through. A *cross-window* view is
   still deferred — task state is per-window, like the rest of the Overlord engine.
2. **Conflict handling.** Two agents in one workspace updating the same task is possible
   but rare; last-write-wins, with `updated_at` making it visible. Note the store persists
   a *whole workspace list* per write, so a writer working from a stale in-memory copy
   overwrites concurrent rows — every mutation path reads and commits synchronously, which
   is what keeps that safe. Anything that mutates after an `await` must re-read first.
3. **Importer collisions.** Resolved: `findDuplicate` (normalized title, scoped to the tab,
   falling back to unclaimed backlog rows) is shared by the importer and `createTasks`, and
   the `origin` rule above decides who may then edit the row.
4. **A tab moved between workspaces** leaves its tasks behind in the old workspace's list.
   Not yet handled — `move_tab_to_workspace` would need to carry them across.

## 9. Rejected

| Rejected | Why |
|---|---|
| Keep mirroring each runtime's private store | Three formats, one undocumented and moving, one scrape-only, one absent. Read-only ceiling. Three silent failures in two days. |
| JSON file the agent reads/writes directly | Wrong host for SSH tabs; needs write reconciliation over the tunnel; no validation or atomicity. Speed advantage is irrelevant next to a model turn. |
| Tasks on `WindowData` (status quo) | A workspace is the project. Window-scoped tasks don't survive moves and don't travel with an exported workspace. |
| Exposing task deletion over MCP | An agent tidying away work it didn't understand is unrecoverable. `done` is enough. |
| Per-tab task storage | Tabs are ephemeral (reload mints a new id); projects are not. Tabs are an *assignee*, not an owner. |
