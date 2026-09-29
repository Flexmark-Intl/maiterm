# Workstream Loom

> Status: built 2026-09-27 as a drawer; moved into the Overlord deck as its home view, with a
> composer and in-place prompts, 2026-09-28. The phone half (maiLink protocol 0.13: task
> `blocker`, `Chat.asks`, `POST /tasks/{id}/answer`) built 2026-09-28.
> Sketch: https://claude.ai/code/artifact/c1508940-a0f8-4e88-bf1c-442143f6733f

## Why

Agents add follow-up tasks, mark work blocked pending a decision, and ask questions while
the human isn't looking. Then a review comes back and the agent carries on, and the
question scrolls away. The task board has held all of this since `docs/tasks.md`, but
nothing drew it as a picture, and a blocker was a lane plus free text, so nothing could
tell "waiting on your decision" from "waiting on eight other tasks".

The Loom is where the human works with the agents without going into their terminals. It is
the **Overlord deck's home view** (Loom · Triage · Board · Ledger), in the Overlord workspace,
and it spans every workspace in the window, with a filter to one. **Cmd+Shift+J** opens the
Overlord workspace on it, board tab in front, and pressed again goes back to the workspace
switched from most recently (G is the editor's find-previous).

It has three modes:

| Mode | What it answers |
|---|---|
| Focus | The maiLink-style chat list; the chosen chat condensed, with what it is stopped at and a composer; that agent's work |
| Weave | Who is working on what, what is waiting and on what, and what has gone quiet |
| Decisions | Every question waiting on the human, oldest first, answerable in place |

**Why the Overlord and not a drawer.** It was first built as a drawer over the terminal area.
That put a second heavy view on top of a live terminal on the webview's one thread, and it
had to fight the hidden terminal for the keyboard (xterm eats Escape, which interrupts the
agent). In the Overlord workspace no other workspace's terminal is on screen, so none paints,
and the deck is an ordinary tab. It is also the place the rest of the supervision already
lives: the Loom absorbed the Fleet view, whose per-agent readouts (state, context, a running
ritual, an unanswered directive with Release) are now Focus's chat header, and its Trigger
menu is the bolt in Focus's composer.
The Loom works with the engine off: it is the human's view, not supervision.

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

## The weave

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

Every active workspace in the window but the Overlord's own, or one picked from the filter
chips. Suspended workspaces are left out: activate one and it appears. "Active" is the
sidebar's rule (`workspace/liveness.ts` `workspaceIsLive`: a terminal tab with a live PTY),
not the `suspended` flag. The flag stays clear on a workspace whose tabs were suspended one by
one, or that was clicked without resuming anything, and filtering on it listed every one of
those.

## Focus

The maiLink inbox rules (`focusSections`): needs you (a permission, or a question on one of
its tasks) → working now → unread or active since the start of yesterday. The chosen chat is
`get_tab_transcript` polled every 3 s, folded by `chatRows`, with **task events** from the
tasks' own records (`taskEventsFor`: added, asked, answered, notes) placed by time and keyed
by their position in the log. The phone's transcript doesn't carry those, and they are the
part that otherwise scrolls away. Agent turns render through `loom/markdown.ts`, a marked
instance that never emits HTML it didn't build: a transcript quotes the web into a webview
that can reach every command. Links open in the browser (WKWebView drops `target=_blank`).

**How the chat reads.** One column at a reading measure (76ch), centered: the agent's prose at
the left edge, the human's messages as bubbles at the right, and everything that isn't
conversation set off by a thin left rule so the eye can skip it:
- tool steps are one quiet line each (verb, then the command cut to one line), the calls on
  click;
- task events say what happened and to which task, and tasks added back to back are one block
  (`chatRows` groups them as an `added` row);
- peer traffic reads "From <peer>" / "To <peer>" with the message as written, folded to four
  lines when long;
- a subagent's report handed back arrives as a "user" turn the harness wrote ("Another Claude
  session sent a message…"); `injectedTurn` folds it to one line, "Subagent report received",
  with the report on click. The phone has a copy (maiLink `src/lib/injected-turn.ts`): change
  the prefix or the labels in both. Turns starting with a tag (`<task-notification>`) never
  arrive: the transcript reader drops them as system noise.

The list and the agent's-work rail are resizable (drag their borders); the widths are kept
per viewer in `localStorage`. The composer carries the terminal composer's Overlord action: a
bolt that runs a rule on this agent by hand, when Overlord is on and a rule applies.

Under the chat, in this order:
- **The prompt the agent is stopped at** (`PromptCard.svelte`): a tool permission with the
  dialog's own rows (read off the screen, `mailink/permission.rs`), an AskUserQuestion with
  its options and an Other field, or Claude's workspace-trust dialog. Answered through
  `answer_tab_prompt_as_human`: the phone's responder and `prompt_id` stale guard, without
  the trust refusal that keeps the Overlord AGENT from trusting folders. It is a separate
  command so nothing an agent's tool call reaches can carry the exemption. BlockerCard's two
  click guards apply: nothing in a prompt's first 1.5 s on screen, never the second click of
  a double-click.
- **A task question** on one of its tasks, through BlockerCard.
- **The composer.** Enter sends, Shift+Enter is a newline; drafts are kept per chat. It sends
  through `send_tab_message`, the phone's `POST /message` rules (an unregistered tab is woken
  first, the trust dialog is never typed at) plus one: **nothing is sent while a prompt is
  open**, because typed text goes into the dialog, a digit picks a row and the rest lands in
  "tell Claude what to do differently".

**"A Claude permission is open" is read off the screen** (`open_prompt` in mailink/mod.rs,
used by the composer, the prompt card, `getTabPrompt` and the responder). The hook's state is
late and long: it arrives 6 s after the dialog opens, and it holds until the approved tool
finishes. Taken from the hooks alone, a message sent in the first seconds went into the
dialog and its CR confirmed the highlighted row; after an approval, a dead card and a refused
composer stayed up for the whole tool run.

**Closing the Board tab doesn't strand the Loom.** `create_overlord_workspace` puts a closed
board back, and `ensureOverlordWorkspace` asks for it whenever the board is missing.

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
- Focus lists a tab only once it has an agent session (or a task question), so a tab sitting
  at the trust dialog before any session, or a dormant one, can't be picked there yet.
