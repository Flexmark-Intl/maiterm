---
title: Tasks
description: One task list per project, owned by maiTerm and shared between you and your agents — six lanes, named workstreams, a per-tab panel, and MCP tools every runtime can use.
---

Agents keep private to-do lists. You watch one scroll past in a terminal, and that's the whole relationship — no way to add an item, correct one, or see across a window what's actually in flight. And every runtime does it differently: Claude Code writes to an undocumented store, Codex buries a plan inside its session log, Gemini has nothing at all.

maiTerm owns that state instead. **Tasks** is one list per project that both sides read and write: your agent records what it's working on through MCP tools, you edit the same rows in a side panel, and each sees the other's changes.

## Tasks belong to the project, not to a session

A task hangs off the **workspace**, because a workspace is a project. That means the work outlives the tab that created it — a tab reload mints a new tab id, a fork mints another, and the tasks stay put and stay attributed. Move a workspace to another window and its tasks travel with it.

A tab is the **assignee**, never the owner. Close a tab and its unfinished tasks are released to the project rather than deleted, where any tab can claim them back.

## Six lanes

```
BACKLOG   TO-DO   ACTIVE   BLOCKED   REVIEW   DONE
   ^         ^
 parked    new work starts here
```

**Backlog is a parking lot, not an inbox.** It's for next month, for future ideas, for the thing you don't want to lose but don't want to think about — and it's exempt from every "is this still in flight?" check, so a deliberately shelved item never ages into a nag. Work you actually intend to start goes in **To-do**, which is where everything new lands. The side panel labels the backlog lane **Parked** for exactly that reason.

A task can also declare that it's waiting on another one. A row with an unfinished prerequisite renders as **Blocked** whatever its stored lane says, and its controls are held while the prerequisite is outstanding — a chip that couldn't move would otherwise report a change that never happened.

## Workstreams

One tab is routinely asked to do two unrelated things, and without any grouping all of it flattens into a single list. A **workstream** is a named job inside a project — "Auth refactor", "DB migration" — while the workspace is still the project.

Agents name their workstream as they go, by name rather than by id, so a job is created on first mention and reused after that; names are deduplicated the same way task titles are, so one job can't become three spellings. Tasks with no workstream are grouped under **No workstream** — shown, never hidden.

## The task panel

Press `Cmd+Shift+E` — or click the lanes icon in the tab bar — to open the task panel beside the terminal. It's the same dock as [notes](/features/workspaces/#per-tab-notes) (`Cmd+E`), per tab, and you can drag its left edge to resize it; the width is remembered.

The panel answers one question — *what am I doing here* — so it shows **this tab's work**, grouped by workstream with a count on each heading. Everything else is a pointer: the header carries collapsed counts for `N unclaimed`, `N parked` and `N done`, and a closing line says how many tasks are in flight on other tabs.

Per row you can:

- **Click the status chip** to advance a task a lane, or shift-click to move it back.
- **Do it** — move the task to Active *and* tell this tab's agent to start on it now. The row then reports what actually reached the agent, because "Active on the board" and "the agent has been told" are different facts: a tab mid-turn, at a permission prompt, or not currently mounted can't be typed into, and the status moves either way.
- **Park** / **Unpark** — shelve a row for later, or bring it back to To-do.
- **Unassign** — hand the row back to the project so any tab can claim it. That's ownership, not status: the task stays in whatever lane it's in.
- **Claim** an unclaimed row, edit its title, write a description, or delete it.

The `+` in the panel header adds a task; each workstream heading has its own `+` that files straight into that job, so the destination is never a guess. Adding opens a small dialog that takes a title, an optional description, and the lane the work starts in — **To-do**, **Active**, or **Parked**. Those are the only three offered: *Blocked* needs something to be blocked on, *Review* needs something to review, and *Done* isn't a state a new task can truthfully be in.

Every one of those edits is visible to the agent through the same tools it writes with. That's what makes the system two-way — until now, task state was something you could only watch.

## What your agent gets

Every agent tab — Claude Code, Codex, local or over SSH — gets three tools:

| Tool | Description |
|------|-------------|
| `listTasks` | List this project's tasks, grouped by workstream. `scope: 'tab'` for just this tab's work, `'workspace'` (default) for the whole project |
| `createTasks` | Create a batch of tasks, optionally into a named workstream |
| `updateTasks` | Update a batch — status, title, detail, workstream, blockers |

They're batched to keep both round trips and token cost down, and every call is scoped to the **calling tab's workspace**, so an agent can never read or write another project's list.

**There is deliberately no delete tool.** An agent may mark a task done; only a human removes one. An agent tidying away work it didn't understand is unrecoverable, and `done` is enough. When you *do* delete a row, the owning agent is told — otherwise it would restate the task on its next list re-send and the deletion would quietly undo itself.

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
