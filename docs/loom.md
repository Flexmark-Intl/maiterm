# Workstream Loom

> Status: loom, decisions and Focus views built 2026-09-27; the phone half (maiLink protocol
> 0.13: task `blocker`, `Chat.asks`, `POST /tasks/{id}/answer`) built 2026-09-28.
> Sketch: https://claude.ai/code/artifact/c1508940-a0f8-4e88-bf1c-442143f6733f

## Why

Agents add follow-up tasks, mark work blocked pending a decision, and ask questions while
the human isn't looking. Then a review comes back and the agent carries on, and the
question scrolls away. The task board has held all of this since `docs/tasks.md`, but
nothing drew it as a picture, and a blocker was a lane plus free text, so nothing could
tell "waiting on your decision" from "waiting on eight other tasks".

The loom is that picture. It opens over the terminal area with **Cmd+Shift+J** (G is the
editor's find-previous) and has three views of the same workspace:

| View | What it answers |
|---|---|
| Loom | Who is working on what, what is waiting and on what, and what has gone quiet |
| Decisions | Every question waiting on the human, oldest first, answerable in place |
| Focus | The maiLink-style chat list, with a condensed chat beside that agent's work |

## The data it stands on

- **Blockers** (`docs/tasks.md` §3.1): `Task.blocker {kind, question, context, options,
  command, asked_at}`. `decision` and `action` blockers are the Decisions queue; waiting on
  other tasks is derived from `blocked_by`, never stored.
- **Answering** is `overlordStore.answerBlocker`, human-only like "Do it". Every surface
  answers through `components/tasks/BlockerCard.svelte`, which carries the two guards:
  - a typed draft belongs to the question it was typed under, and a re-ask drops it;
  - nothing sent without visible typed text (an option, "I've done it", Enter in an empty
    field) is accepted while the question is under 1.5 s old, measured from its own
    `asked_at`. A card can mount with a new question right under the pointer.
- **Chat** (Focus): `get_tab_transcript` serves `mailink::tab_transcript`, the same turns
  the phone's thread gets. `loom/model.ts` `chatRows` folds tool runs with the phone's
  vocabulary (`toolVerb`), and `focusSections` applies the phone's Focus rules.

## The loom view

- **Left: agents.** Every agent tab in scope, plus any tab still holding open tasks.
  Ordered permission → working → idle → no session, then by name, so the column doesn't
  reshuffle as agents finish turns. Clicking one fades everything it isn't part of.
- **Middle: workstreams.** Open tasks only (backlog is the parking lot, retired is
  history). Streams with a question waiting on the human come first. A chip untouched for
  14 days while claiming to be active or to-do is drawn quiet (`isQuiet`): the board had
  month-old "active" rows that looked exactly like live work.
- **Strings** are drawn in one SVG over the grid from the DOM rects, recomputed on a
  ResizeObserver, on scroll of the stream column, and on any data change. Moving dots on a
  string mean the task is active; a slow amber dash running backwards means it is waiting;
  a faint dotted string joins a task to what it depends on. Strings to chips scrolled out
  of view are skipped.
- **Right: the selected task.** Lane, workstream, agent, age; the blocker card with its
  answer controls, or the unmet dependencies, or "no reason recorded"; detail; the log.

## Scope

A drawer in every workspace, over the active workspace by default, with a switch to every
workspace in the window. The Overlord workspace always shows the whole window: it
supervises all of it.

## Focus

The maiLink inbox rules (`focusSections`): needs you (a permission, or a question on one of
its tasks) → working now → unread or active since the start of yesterday. The chosen chat is
`get_tab_transcript` polled every 3 s, folded by `chatRows`, with **task events** from the
tasks' own records (`taskEventsFor`: added, asked, answered, notes) placed by time and keyed
by their position in the log. The phone's transcript doesn't carry those, and they are the
part that otherwise scrolls away. Agent turns render through `loom/markdown.ts`, a marked
instance that never emits HTML it didn't build: a transcript quotes the web into a webview
that can reach every command. Links open in the browser (WKWebView drops `target=_blank`).

**The drawer holds the keyboard while open.** It covers the terminal, so a focused terminal
underneath is a hidden one: xterm eats Escape (interrupting the agent) and typing lands where
nobody can see it. Focus landing in the terminal area goes back to the drawer. Closing
returns focus to what had it when the drawer opened if that tab is still on screen, else to
the terminal on screen, never to a tab switched away from.

## Blocked with no reason recorded

A Blocked task with no blocker record and no unmet dependency has its question only in the
agent's scrollback (`unexplainedBlocked`; imported rows are left out, since their
dependencies live in the runtime's own store). Decisions lists them under the queue with
**Ask for the reason** (`overlordStore.askForBlockerReason`), which types one question at
the carrying agent, human-only like "Do it".

It is a button and not an Overlord rule on purpose. A rule was built and reverted
(9a44aaf, 1b701f6):
- **Downgrade.** Its condition was a new variant of the persisted `OverlordCondition` enum,
  and every seeded default is written into every user's state file on first launch. An older
  build that met the unknown variant failed to parse the WHOLE file, fell back to an empty
  state, and later overwrote the backup.
- **False alarms.** It fired hourly on Claude's own task dependencies, which import as
  Blocked with an empty `blocked_by`.

Do not add a variant to a persisted serde enum without first shipping a lenient reader in an
earlier release.

## Next

- A push when an agent asks a new question: a doorbell kind on the desktop, plus its relay
  `KIND_BODY` line.
- Exercise Focus live on a build with registered agent sessions (dev HMR empties the stores).
