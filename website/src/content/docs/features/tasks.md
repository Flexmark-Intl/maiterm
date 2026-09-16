---
title: Tasks
description: One kanban board per project, owned by maiTerm and shared between you and your agents — seven lanes, named workstreams, real dependencies, a per-tab panel, and MCP tools every runtime can use.
---

Agents keep private to-do lists. You watch one scroll past in a terminal, and that's the whole relationship — no way to add an item, correct one, or see across a window what's actually in flight. And every runtime does it differently: Claude Code writes to an undocumented store, Codex buries a plan inside its session log, Gemini has nothing at all.

maiTerm owns that state instead. **Tasks** is one list per project that both sides read and write: your agent records what it's working on through MCP tools, you edit the same rows in a side panel, and each sees the other's changes.

## Tasks belong to the project, not to a session

A task hangs off the **workspace**, because a workspace is a project. That means the work outlives the tab that created it — a tab reload mints a new tab id, a fork mints another, and the tasks stay put and stay attributed. Move a workspace to another window and its tasks travel with it.

A tab is the **assignee**, never the owner. Close a tab and its unfinished tasks are released to the project rather than deleted, where any tab can claim them back.

## Seven lanes

```
BACKLOG   TO-DO   ACTIVE   BLOCKED   REVIEW   DONE      DROPPED
   ^         ^                                 ^           ^
 parked    new work starts here            the flow   retracted, off the flow
```

**Backlog is a parking lot, not an inbox.** It's for next month, for future ideas, for the thing you don't want to lose but don't want to think about — and it's exempt from every "is this still in flight?" check, so a deliberately shelved item never ages into a nag. Work you actually intend to start goes in **To-do**, which is where everything new lands. The side panel labels the backlog lane **Parked** for exactly that reason.

**Dropped is retraction, not completion.** An agent that filed work it had misread used to have exactly two exits, and both lie: *Done* claims it finished — and satisfies every dependent, so a task legitimately waiting on the retracted one silently became ready work — while *Backlog* claims it was deliberately deferred, and parked rows are exempt from every staleness check, which makes it a quiet place to hide a mistake. Deletion stays human-only, because an agent tidying away work it didn't understand is unrecoverable. The missing verb was never *delete*, it was **retract**.

So a dropped row **does not satisfy a dependent** — an agent cannot unblock its own task by dropping the one it was waiting on. The dependent stays blocked, and when an agent reads the board it is told which row it is waiting on and the lane that row is in, so a retraction surfaces as a real question rather than work quietly starting. It is counted separately from *Done*, because "12 done" must never include four tasks nobody did. And it is reversible: a lane, not a delete, so the card stays reachable and you can drag it back out. It sits off the flow for that reason — the steppers walk the six, and reaching *Dropped* is always a deliberate act rather than one click past *Done*.

## Dependencies

A task can declare that it's waiting on another one. A row with an unfinished prerequisite renders as **Blocked** whatever its stored lane says — unless it has already been retired to *Done* or *Dropped*, which keep their own lane. While it's held, every control that would move it is held with it, on the board and in the panel alike: the status chip, the steppers, **Park** and **Do it**. A control whose label can't move must not move the value either.

Both surfaces say **why**, in the same words: which task this one is waiting on, and the lane that task is in. That last part is the difference between "waiting on Retry queue" meaning *it's coming* and meaning *it never will* — a **dropped** prerequisite is retracted work that nobody intends to do, so the row is told plainly that it will not clear itself rather than being promised a clearance that can't arrive.

A prerequisite that was [archived along with its tab](#archiving-carries-the-work-with-the-tab) is named too, with the tab it's parked on, so it's something you can actually act on rather than a dependency on a task nobody can find.

For agents, the same edges are readable and writable one at a time: `listTasks` returns each blocker as a title, a lane and whether it's met, waiting, parked or gone, and `blocking` names what's waiting on *this* task — the thing an agent needs to know before it goes idle. Edits are incremental (`block_on` / `unblock_from`) rather than a whole-array replace, so two agents editing dependencies on the same board don't clobber each other. maiTerm refuses an unknown prerequisite and a task blocked on itself: both record an edge that reads as real and either does nothing or parks the row forever.

## An append-only log per task

A row sitting in **Blocked** with nothing on it saying why is the most useless card on the board. The task's **description** is a spec — edited, rewritten, replaced — so an agent that wanted to record *why* it stalled had to destroy whatever was already there.

Notes are a separate, append-only log: one line at a time, shown on the board card and under the spec on an expanded row in the panel, and never edited afterwards. Who wrote it is stamped by maiTerm rather than claimed by the writer, because "the agent says it's blocked on the migration" and "I wrote that down" are different claims about the same row. The log is capped at the twenty most recent entries, so an agent in a retry loop can't grow a task without bound.

## Workstreams

One tab is routinely asked to do two unrelated things, and without any grouping all of it flattens into a single list. A **workstream** is a named job inside a project — "Auth refactor", "DB migration" — while the workspace is still the project.

Agents name their workstream as they go, by name rather than by id, so a job is created on first mention and reused after that; names are deduplicated the same way task titles are, so one job can't become three spellings. Tasks with no workstream are grouped under **No workstream** — shown, never hidden.

## The task panel

Press `Cmd+Shift+E` — or click the lanes icon in the tab bar — to open the task panel beside the terminal. It's the same dock as [notes](/features/workspaces/#per-tab-notes) (`Cmd+E`), per tab, and you can drag its left edge to resize it; the width is remembered.

The panel answers one question — *what am I doing here* — so it shows **this tab's work**, grouped by workstream with a count on each heading. Everything else is a pointer: the header carries collapsed counts for `N unclaimed`, `N parked`, `N done` and `N dropped`, and a closing line says how many tasks are in flight on other tabs.

![The task panel beside a terminal, showing one workstream's rows with review, active and blocked lanes and a count of finished work in the header](/screenshots/tasks-panel.webp)

Per row you can:

- **Click the status chip** to advance a task a lane, or shift-click to move it back. It's held on a row that's blocked by an unfinished prerequisite, along with everything else that would move that row.
- **Do it** — move the task to Active *and* tell this tab's agent to start on it now. The row then reports what actually reached the agent, because "Active on the board" and "the agent has been told" are different facts: a tab mid-turn, at a permission prompt, or not currently mounted can't be typed into, and the status moves either way.
- **Park** / **Unpark** — shelve a row for later, or bring it back to To-do.
- **Unassign** — hand the row back to the project so any tab can claim it. That's ownership, not status: the task stays in whatever lane it's in.
- **Claim** an unclaimed row, edit its title, write a description, or delete it.

The `+` in the panel header adds a task; each workstream heading has its own `+` that files straight into that job, so the destination is never a guess. Adding opens a small dialog that takes a title, an optional description, and the lane the work starts in — **To-do**, **Active**, or **Parked**. Those are the only three offered: *Blocked* needs something to be blocked on, *Review* needs something to review, and *Done* isn't a state a new task can truthfully be in.

Every one of those edits is visible to the agent through the same tools it writes with. That's what makes the system two-way — until now, task state was something you could only watch.

The same list is editable from your phone: [maiLink](/features/mailink/#what-you-can-do-from-the-phone) shows a tab's rows and lets you add, retitle, re-lane or reassign one, and the write lands in maiTerm even while the Mac's screen is asleep.

## What your agent gets

Every agent tab — Claude Code, Codex, local or over SSH — gets three tools:

| Tool | Description |
|------|-------------|
| `listTasks` | List this project's tasks, grouped by workstream. `scope: 'tab'` for just this tab's work, `'workspace'` (default) for the whole project |
| `createTasks` | Create a batch of tasks, optionally into a named workstream |
| `updateTasks` | Update a batch — status, title, detail, workstream, assignee, dependencies, and an appended note |

They're batched to keep both round trips and token cost down, and every call is scoped to the **calling tab's workspace**, so an agent can never read or write another project's list.

### "What can I actually start?"

Scope used to be the only lever, so every call came back with the whole project — and a project only grows, since finished rows are never swept. `listTasks` now answers the question it was always being asked:

- **`ready`** — not retired, not parked, nothing unmet blocking it, and either this tab's or unclaimed. The dependency part is exactly why this belongs in the tool rather than in the agent's head: an agent can't tell whether a prerequisite on another tab has landed.
- **`status`** — matches the lane the board actually shows, not the stored one, so a call can't omit a task the same call reports as blocked.
- **`limit`** — 100 by default, and rows are **ranked before they're cut** (active, then blocked, review, to-do, backlog, done, dropped). Cutting in stored order drops whatever happens to be last, routinely the task in flight, while a year of finished rows survives above it. A shortened list says so and says how to reach the rest, because a silently short list is indistinguishable from a small project.

A narrowed call still carries the full list of workstream names, so an agent that filtered can't invent a second spelling of a job it couldn't see.

### Handing work to another tab

An agent can assign a task to another tab, claim an unclaimed row, or release one of its own. The hand-off lands on the board — silently, always — and the *notice* goes to whoever may act on it: when [Overlord](/features/overlord/) is running, it raises a card naming the task and the tab, and if the target tab is exempt from supervision no card is raised at all.

What doesn't happen is one agent typing into another agent's terminal. A tab can't tell an injected line from something you typed, so cross-tab injection carries your authority and stays with the tools that have it — Overlord's `driveTab` and the panel's own **Do it** button. An agent able to notify a peer by writing one field of a task update would hold that authority for the price of an ordinary edit.

The reply says which of those happened, in words. "Nobody was told" isn't a failure — the row is assigned, it's on the board, it's on the target's panel — but the caller is told so, because the alternative is an agent that believes it delegated the work and stops tracking it.

**There is deliberately no delete tool.** An agent may mark a task done, or *retract* one to **Dropped** when it filed work it had misread; only a human removes a row. An agent tidying away work it didn't understand is unrecoverable, and between those two it has an honest exit either way — which is why the tool description tells it to drop such a task rather than close it as done. When you *do* delete a row, the owning agent is told — otherwise it would restate the task on its next list re-send and the deletion would quietly undo itself.

### Agents pick it up on their own

Every agent tab is told about the list when its session starts, resumes, forks or compacts — including *if you already have a task or todo list for this project, move it here*. That clause matters because resume, fork and compaction are exactly when an agent is mid-project holding a live list: without it, adoption would wait for the agent's next multi-step job and everything already in flight would stay invisible. Finished items are left behind rather than migrated, so a fresh board doesn't fill with history.

For Claude Code there's a second, free route: maiTerm reads that runtime's own task store one-way and mirrors it in, marked as imported. A Claude tab that never learns the tools still shows up. Nothing is ever written back, and an imported row is never allowed to overwrite work you or an agent edited directly.

## Archiving carries the work with the tab

[Archiving a tab](/features/terminal/#archive-and-restore) parks its tasks with it — they leave the project's list and travel on the archived tab record — and restoring the tab brings them back, still attributed to it. A prerequisite parked this way still blocks whatever was waiting on it, rather than quietly reporting that work as ready.

Archiving is the reversible disposition, so it never releases a tab's rows. Deleting an archived tab is the irreversible one, and it destroys them along with it.

## Turning it off

Tasks is **on by default**. The switch is **Preferences → AI Agents → Task tracking**: turn it off and agents get neither the tools nor the instruction, and nothing is said to them about any of this.

:::note
The task panel is per tab; the whole-window view — every workstream across every workspace, with cards you drag between lanes — is the **Board** in [Overlord](/features/overlord/). Overlord is a consumer of this list, not its owner, and Tasks works with Overlord switched off.
:::
