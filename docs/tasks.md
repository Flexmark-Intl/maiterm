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

> **The phone is a writer too** (2026-09-08, `docs/mailink-protocol.md` §13.3). maiLink reads the
> board over `GET /tasks` and per-tab rows on `chat_detail.tasks` + the WS `tasks` event, and
> writes with `POST /tasks` and `POST /tasks/{id}`. Two things about it are load-bearing here:
>
> - **Those writes go into Rust state directly, then EMIT `mailink-tasks-changed`** so the
>   frontend store replaces its copy. It persists WHOLE lists, so a row it never saw would be
>   clobbered by its next edit. Routing the write through the webview instead was the first
>   design and was wrong: the phone's whole use case is a sleeping desktop, and a webview whose
>   screen is off throttles its timers.
> - **Setting a lane and telling an agent are different acts, and only a human may do the
>   second.** `POST /tasks/{id} {status:"active"}` is silent, permanently — that is what an agent
>   uses to mark its own row as it picks work up. The board's "Do it" is a separate verb,
>   `POST /tasks/{id}/start`, which also types the notice. If the notice were inferred from the
>   transition, every agent that marked its own row Active would type "please pick up this task
>   now" at itself, mid-turn, about the work it is already doing. A distinct endpoint carries the
>   human's intent in its name and is unreachable by an agent updating its status.

```
Workspace.tasks[]          ← source of truth (a workspace IS a project)
  ├─ MCP: listTasks / createTasks / updateTasks   ← agents, every runtime, incl. SSH
  ├─ Side panel (per tab + workspace backlog)     ← humans
  ├─ maiLink (the phone)                          ← the human, remotely — see below
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

**Archiving is not a close, so it does not release.** A closed tab is gone; an archived one
is coming back if anyone wants it, and it keeps its scrollback, cwd and ssh context for
exactly that reason. Releasing its rows would have made the one reversible disposition
irreversible for the work: `tab_id` cleared is not recoverable, so restoring the session
returned a tab whose tasks were now indistinguishable from everyone else's unclaimed rows.

So `archive_tab` MOVES the tab's rows out of `Workspace.tasks` onto the archived tab record
(`Tab.archived_tasks`), and `restore_archived_tab` moves them back, still attributed. They
are stored on the tab rather than flagged in place so that nothing which reads
`Workspace.tasks` — board, panel, counts, staleness rules, the untracked check — needs to
know they exist. A row that isn't in the list cannot be shown by something that forgot to
filter it. Both paths call `tasksStore.rehydrate()` afterwards: the frontend mirror persists
whole lists, so a stale copy would put the moved rows straight back.

Deleting an archived tab destroys its rows along with it — that is what makes it the
irreversible one.

Three things follow from "off the list but not gone", each of which was wrong first:

- **A workstream is kept while any task points at it, including a parked one.**
  `set_workspace_tasks` garbage-collects workstreams with no referent, so moving a tab's rows
  off the list left a workstream whose only members were that tab's — and the next task write
  in that workspace deleted the label permanently. Restore then dropped the rows into
  "Ungrouped", and the board's rename silently refused to fix it, because `renameWorkstream`
  no-ops on an id that no longer exists.
- **A parked prerequisite still blocks.** `hasUnmetDeps` treats an unresolvable `blocked_by`
  id as met, which is right for a *deleted* prerequisite and wrong for a parked one: archiving
  the tab holding "migrate schema" moved everything waiting on it into To-do and reported it
  to agents as ready work. It now takes a `parked` set (`workspacesStore.parkedTaskIds`);
  unresolvable-and-unknown means gone, unresolvable-but-parked means waiting.
- **Rows the tab left in another workspace are released, not moved.** A tab dragged between
  workspaces leaves rows behind, and `archive_tab` only partitions the workspace it is
  archiving from. The rest have their `tab_id` cleared — as the old archive path did for all
  of them — because a row attributed to a tab that is not in that workspace is neither `mine`
  nor `unclaimed` in its panel, and nothing can reach it again.

**The mirror is patched synchronously, not rehydrated.** The frontend store persists WHOLE
lists, so it must not be stale for a single await: any writer in the gap computes its write
from the pre-move copy, which on the archive side puts the rows back on the board while the
archived tab also holds them (restore then duplicates the ids, and Svelte's keyed `each`
throws), and on the restore side drops them from both places with no copy anywhere.
`restore_archived_tab` therefore returns the rows on the returned tab — transport only, the
stored one is cloned with the field already empty — so `applyTabArchive`/`applyTabRestore` can
update the mirror in the same synchronous step, without persisting.

On the archive side the patch goes **before** the invoke, not after. Tauri runs these sync
commands in the order the webview sent them, so a `commit()` issued after the patch carries
the post-move list and still lands after `archive_tab` — correct either way. Patching after
the await instead left the mirror stale for the whole flight of `archive_tab`, which is a real
window: a 5s Overlord tick and any agent's MCP task call are both writers. A failed archive
rehydrates, since the mirror then claims something Rust never did.

**Two things the local copy has to carry.** `archiveTab` builds its local archived-tab record
by spreading the LIVE tab, which never has `archived_tasks` — so `parkedTaskIds` stayed empty
for anything archived in the current session, and the parked-dependency fix did nothing until
the next app start re-read the archive from disk. `applyTabArchive` returns the rows it
removed, and they go onto that record. And it mirrors the cross-workspace release too: rows
left in a workspace the tab was dragged out of get `tab_id` cleared locally as well, or the
next whole-list write to that workspace re-attributes them.

**Every consumer of the parked set has to use it.** `effectiveStatus` decides the lane;
`hasUnmetDeps` also feeds the board's `depBlocked`, which disables the ‹ › steppers. When only
the first was given the parked set they disagreed: the card rendered in Blocked with its
steppers live, and four clicks walked the *stored* status to `done`, where `effectiveStatus`
short-circuits — a card jumping to Done for work that never started.

### The lanes, and what `backlog` and `dropped` actually mean

```
BACKLOG   TO-DO   ACTIVE   BLOCKED   REVIEW   DONE  │  DROPPED
   ^         ^                                      │     ^
 parked    new work starts here                     │  retracted
           └────────── the flow ──────────┘         │  (off it)
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

#### `dropped` — retracted, not finished (2026-09-10)

Six lanes gave an agent that had filed work it misread exactly two exits, and both lie.
`done` says it finished — and, worse, **satisfies every dependent**, so a task legitimately
waiting on the retracted one silently became ready work. `backlog` says it was deliberately
deferred, and parked rows are exempt from every staleness check, which makes it a quiet
place to hide a mistake. Deletion is human-only and stays that way (§9): an agent tidying
away work it didn't understand is unrecoverable. So the missing verb was never *delete*, it
was **retract**.

Four properties, each of which is a place the six-lane code was wrong:

- **It does not satisfy a dependent.** `hasUnmetDeps` counts only `done` as met, so this
  falls out — and it is the property that stops `dropped` becoming a back door: an agent
  cannot unblock its own task by dropping the one it was waiting on. The dependent stays
  blocked and `resolveBlockers` names the row and its lane, so the human sees a real
  question rather than work quietly starting.
- **It is retired, so it is swept.** `isRetired` = `done || dropped`, and the retention
  sweep (48h, machine-authored rows only) reads it. A retracted row has less reason to
  linger than a finished one.
- **It is not parked.** A parked idea is still coming; this is not. The panel and board
  count them separately — folding `dropped` into `done` would let "12 done" include four
  tasks nobody did, which is the one number on the board that must not lie.
- **It is reversible.** A lane, not a delete, so the card stays reachable and a human can
  drag it back out (or press ↑ in the panel). That is what keeps "an agent may retract"
  from meaning "an agent may disappear work".

**It is a lane but NOT a step in the flow.** `FLOW_STATUSES` is the six; `TASK_STATUSES` is
all seven. The steppers and the panel's status chip walk the flow only — putting `dropped`
in the cycle would make one click past DONE mean "this should never have existed", the worst
adjacency in the vocabulary. Reaching it is a decision: an agent's `updateTasks`, or a human
drag. Its own steppers are off, and they say why.

**The importer had to learn it too.** `syncMirrorTasks` drove any row it owned toward
whatever the runtime's private store still said, so the 5s tick dragged a dropped row back
to `todo` — forever. It now skips retired rows outright. Same defect existed for `done`,
where it re-opened rows somebody had just closed; the origin rule answers "who may drive
this row", and that is a different question from "is this row still running".

`createTasks` deliberately does not advertise `dropped` in its schema — you retract
something that exists. The clamp accepts it, so nothing breaks if an agent sends it.

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

updateTasks({ updates: [{ id, status?, title?, detail?, note?, workstream?,
                          blocked_by?, block_on?, unblock_from?,
                          assign_to? }] })                 // tab id | "me" | null
  → { updated: string[], missing: string[], refused?: [{ id, reason, detail }] }
```

### `detail` is the spec; `notes` is the log (2026-09-10)

`detail` is a whole-field replace, so an agent recording why something was blocked had to
read it, rewrite it, and destroy whatever reasoning was there. There was no other place for
it — `replyToOverlord` carries `blockers[]`, but that is a window-level escalation which
does not attach to the task, so **the board showed rows sitting in Blocked with nothing on
them saying why**. That is now the first thing a card's expansion shows.

`Task.notes: Vec<TaskNote{at, text, by}>`, appended via `updateTasks`'s `note`. Four
decisions:

- **A separate field, not a `detail` convention.** The two answer different questions and
  have different lifetimes: the spec is edited, the log is only ever added to.
- **Capped at `TASK_NOTE_CAP` (20), newest kept.** An agent in a retry loop appends forever,
  and the store persists a WHOLE workspace list on every task write — so an unbounded field
  is paid for by every unrelated write in the project too. Trimmed on append and again by
  Rust before disk, the same defense-in-depth as `normalized_title` and the status clamp.
- **`by` is stamped, never taken from the caller.** "The agent says it is blocked on the
  migration" and "I wrote that down" are different claims about the same row.
- **`listTasks` returns the TAIL, not the log.** Three notes at `scope: 'workspace'`, which
  carries every row in the project; the whole log at `scope: 'tab'`, where narrowing is
  already the ask and it costs nothing.

**`assign_to` (2026-09-10) — an agent could not assign anything, including to itself.**
The phone wrote `tab_id` through `board::UpdatePatch` and the side panel claimed and
unassigned, but MCP had no assignee field at all: no hand-off, no claiming an unassigned
row, no releasing one. The only route was a side effect — `createTasks` restating a title
so `findDuplicate` reclaimed the row — which is invisible in the tool description, works
only when `tab_id` is already null, and reads as "create" while doing "claim".

Three constraints:

- **The target must be a tab in the caller's workspace.** Reaching outside would put a row
  on a tab whose panel cannot show it (the panel reads one workspace's list) and whose
  close would never release it, since `releaseTab` looks for the tab that closed. Exactly
  the unreachable state `archive_tab`'s cross-workspace release exists to prevent.
- **Absent and `null` are different answers.** Absent leaves the assignee alone; `null`
  releases the row. Same rule the phone's patch already follows.
- **A refusal is reported, never swallowed.** `refused: [{id, reason, detail}]` — an agent
  that hands work to a tab and is told nothing believes the hand-off happened and stops
  tracking the task. The assignee is resolved before anything else in the update is
  applied, so a refused hand-off cannot half-write the rest of the same row.

Assigning does not TELL the tab anything — see §5's delegation note. Setting a lane and
telling an agent are separate acts, and that is deliberate.

`listTasks` returns tasks **grouped by workstream** rather than flat — a flat list invites
an agent to treat two separate jobs as one, which is the thing workstreams exist to stop.

### Dependencies are legible, and edited one edge at a time (2026-09-10)

`blocked_by` was three defects wearing one field:

- **Raw ids.** `blocked_by: ["a3f8…"]` meant scanning the whole list to learn what the task
  waited on — and on a `scope: 'tab'` list the prerequisite is usually on another tab and
  not in the payload at all, so the id resolved to nothing the agent could see. It now comes
  back as `[{id, title, status, state}]`, where `state` is the part that decides anything:
  `met` finished, `waiting` live, `parked` off-list with an archived tab (still blocks),
  `gone` deleted (does not). `resolveBlockers` draws the parked/gone line in exactly the
  same place `hasUnmetDeps` does — if those two ever disagree, a task renders as blocked by
  something the dependency check has already released, or the reverse.
- **Whole-array replace only.** Two agents editing dependencies clobbered each other, since
  the store persists a whole workspace list per write (§8.2). `block_on` and `unblock_from`
  add and remove single edges. When all three arrive together `blocked_by` is the base and
  the incremental edits apply on top, rather than one silently winning.
- **No reverse edge.** Nothing answered "what is waiting on me", which is what an agent
  needs before it goes idle. `blocking` carries it, and is absent on the rows — almost all
  of them — that block nothing.

Two refusals, both because the alternative is an edge that lies:

- **An unknown blocker id.** `hasUnmetDeps` treats an unresolvable id as met, so a typo'd id
  would record a dependency that does nothing while reading back as a real one. Parked ids
  are accepted: off the list, still blocking.
- **A self-edge.** Never met, so the row would sit in Blocked forever.

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

Content: this tab's tasks grouped by workstream, click-to-edit title/detail, status
cycling, park/unpark, "Do it", claim/unassign, delete, a blocked-by indicator, and
`N parked` / `N done` / `N unclaimed` collapses. One closing line reports how many tasks
are **in flight on other tabs** — a pointer to the board, never a list, and never a raw
row count (that grew with every task the project ever finished, so it shouted loudest
when nothing was happening).

**Workstream headings are unconditional** (2026-09-01). They were hidden below two groups,
on the reasoning that a lone heading labels something with nothing to distinguish it from.
That treats a workstream as a *divider*; it is the name of the **job** the rows belong to,
and that is context the reader needs whether or not a second job is on screen — five rows
under no heading do not say which of your jobs this tab is on. Each heading carries its
visible-row count and its own **add** button, and hiding them also hid that button exactly
when a tab was doing one thing, which is most of the time.

### Adding a task

The inline "Add a task…" field is **gone** (`4edcf31`, `467d522`). It could carry only a
title, and it inferred the workstream — one job in flight and the task joined it, two or
more and it went loose — without ever showing which. Both outcomes were invisible at the
moment of typing, so you learned where a task went later, from the board. The inference
was never the bug; the silence was.

Adding is now a modal (`TaskAddModal.svelte`) carrying **title, description and starting
lane**, opened from either:

- a **workstream heading's `+`** — destination comes from which button was pressed, so
  nothing has to ask, which is what lets the no-picker rule survive; or
- the **panel header's `+`** — the always-available way in, using the same inference the
  old field used, with the destination now named in the modal's subtitle. It is not
  redundant with the heading buttons: a tab with no tasks has no groups, hence no headings
  and no buttons at all, and a tab whose rows are all done or all parked with those filters
  off is in the same state.

Only `todo` / `active` / `backlog` are offered. `blocked` needs something to be blocked on,
`review` needs something to review, and `done` is not a thing you create.

Two traps this created, both found by review:

- **`tasksStore.add` is idempotent by normalized title** and returns the existing row with
  a patch carrying neither `detail` nor, usually, `status`. Ignoring that return threw away
  everything the modal exists to collect — and worse than a no-op in the common case, since
  the button only appears with 2+ groups and one is usually *No workstream*: adding a title
  that matches a LOOSE row files **that** row into the named job, so it vanishes from one
  heading and reappears under another with no description, still in To-do. Compare ids;
  apply the lane either way; fill the description only when the row has none.
- **`autofocus` is not focus.** Svelte compiles it to a microtask that focuses only if
  `document.activeElement === body` — true when a mouse opened the modal (WebKit does not
  mouse-focus buttons), false from the keyboard, where the `+` keeps focus. That button is
  *outside* the backdrop, and the Escape handler is *on* the backdrop, so keyboard users got
  no typing, no Escape, and a mouse-only exit. Focus explicitly on rAF.

### Row actions, and the two axes they act on

`↥` / `↧` write **`Task.tab_id`** and nothing else. A task handed back stays in its lane and
merely stops belonging to this tab. Park writes **`Task.status`**. These are different axes,
and labelling the first "Hand back" described an intent rather than an effect — since it was
also the only control on the row writing ownership rather than status, it read as whichever
one you expected. It now says *"Unassign — drop this tab's claim… Stays in its current lane;
this is not the same as parking it."*

**"Do it"** moves the row to `active` and types a notice at the tab carrying it
(`overlordStore.startTask`), naming the task, its detail and its id. Two rules:

- It is **not** gated on `overlord_enabled`, unlike the delete notice. The human clicked it
  and the text goes to the tab they were looking at, so this is the human typing, not maiTerm
  acting for a supervisor that has been switched off. Only the relay-when-unreachable
  fallback belongs to Overlord, and it is skipped when there is no agent tab.
- The row reports **what actually reached the agent**. "Active on the board" and "the agent
  has been told" are different facts — the tab can be mid-turn, at a permission prompt, or
  unmounted, all of which refuse a paste — and a button that implies the second while doing
  only the first is how a task sits Active for an hour with nobody working on it. The status
  moves either way: the human has said what they want, and that stays true whether or not a
  paste could land at that instant.

**All three are held while a prerequisite is unfinished.** They write the STORED status
while the row displays the EFFECTIVE one, so on a dependency-blocked row the chip does not
move: "Unpark — back to To-do" left it reading BLOCKED, and Do it told an agent to start work
whose prerequisite had not landed, with nothing on screen changing. This is verbatim what the
board already guards with `PINNED_WHY` — and unlike the board's steppers, Do it also emits a
terminal injection.

**`N unclaimed` is not a scope violation, it is the only way a HUMAN reaches that work.**
The panel is the only surface a person has that writes `Task.tab_id` — the board reassigns
workstream and status and never the assignee. So without a claim control, a row released by
`releaseTab` when its tab closed would be unreadable, uneditable and undeletable from every
human surface, forever, while still inflating counts. (Since 2026-09-10 agents write it too,
via `updateTasks`'s `assign_to`. That does not retire this control: the row that strands here
is precisely the one no agent restates.)
Unclaimed work is also genuinely this panel's business: it is the pile you can pick up
*here*, not another tab's work. Collapsed by default; ↧ claims, ↥ unassigns. There is
deliberately no way to add **into** the unclaimed pile: every add claims for this tab, and a
row becomes unclaimed by being released from one or by outliving the tab that held it. That
group is also the one heading with no add button — it is an *assignment* bucket whose rows
come from every job at once, so no destination could be meant by pressing one.

Removed, and why: it also listed **the rest of the project**, offered a **workstream
picker** on every add, and a **per-row workstream dropdown**. All three are *organizing*
work, and organizing has a proper home now — the board
is indexed by workstream (`docs/overlord.md`), where a drag moves a task between jobs and
the whole window is legible at once. Reproducing that in a 280px dock made the panel a
worse board and buried the one list the tab actually needs.

The add modal is **not** that picker returning: it never asks where the task goes, it
*states* it, because the destination came from which button opened it.

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
