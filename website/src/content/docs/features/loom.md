---
title: Workstream Loom
description: Work with every agent in a window without going into their terminals — a condensed chat with each one, the questions they're waiting on, and a picture of who is working on what.
---

Agents ask questions while you aren't looking. They add follow-up tasks, mark work blocked pending your decision, stop at a permission prompt — and then a review comes back, the agent carries on, and the question scrolls away. With a dozen of them, finding out what anyone is waiting for means visiting a dozen terminals.

The **Workstream Loom** is where you work with your agents without going into their terminals. It spans every workspace in the window, and it has three modes: **Focus** to talk to the agents, **Weave** to see who is working on what, and **Decisions** for every question waiting on you.

## Opening it

Press `Cmd+Shift+J`. The Loom is the home view of the [Overlord](/features/overlord/) deck — the first of its four views, **Loom · Triage · Board · Ledger** — so maiTerm switches to the Overlord workspace with the Loom in front. Press `Cmd+Shift+J` again and you're back in the workspace you came from.

It works with Overlord's engine switched off: the Loom is your view of the agents, not supervision. With Overlord on, the **♔ Overlord** row in the sidebar opens the same deck.

Living in its own workspace is deliberate. No other workspace's terminal is on screen while you're there, so nothing paints behind it, and nothing is drawn over a live terminal that could steal its keyboard.

### What it covers

Every **awake** workspace in the window — the sidebar's idea of awake, one with a live terminal in it. A suspended workspace is left out until you wake it. With more than one in scope, filter chips across the top narrow the Loom to a single workspace, or **All**. Beside the modes, a summary counts what's active, what needs you, what's waiting, and what has been quiet for 14 days or more.

## Focus

Focus is the work area: the chat list on the left, the chosen agent's conversation in the middle, and that agent's work on the right. Drag either border to resize the columns; the widths are remembered.

**The chat list** follows the same rules as the [maiLink](/features/mailink/) inbox, in three groups: **Needs you** (a permission prompt, a question on one of its tasks, or a watch script waiting to be allowed), **Working now**, and the agents active recently. How far back that last group reaches is a menu on its own heading — **Today**, **Since yesterday** (the default), **Last 3 days**, **Last 7 days** or **Last 30 days** — and your choice is remembered. Agents that need you or are working show up whatever it's set to. Each row shows how long ago the agent last did anything, and a line saying what it's waiting on you for or what it's doing right now.

**The chat** is a condensed transcript — the same one your phone gets — in one readable column. The agent's replies on the left, yours as bubbles on the right, and everything that isn't conversation set off so the eye can skip it: each tool step is one quiet line (click for the calls), messages to and from [peers](/features/mesh-workspace/) say who they're from, and a subagent's report folds to one line. So does a message maiTerm typed into the agent rather than you — an Overlord rule's directive reads **Overlord**, followed by the rule's name, and a task answer or anything else sent on your behalf reads **Sent for you** — so your own bubbles hold only your own words. Click one to read what was sent. Task events are woven in where they happened — a task added, a question asked, an answer given — because those are exactly the parts that otherwise scroll away. Clicking one opens the task in the Weave. You can select and copy anything in the chat.

**The header** shows the agent's state, its model, reasoning effort and a context gauge, any Overlord sequence running on it, and **Open the tab** to go to its terminal. On a Claude Code chat the model and the effort are menus: pick one and maiTerm sends `/model` or `/effort` for you, the same way the phone does. The header changes when the agent replies on the new model, not when you pick it, since Claude Code can refuse a switch. When a directive is waiting for an answer that isn't coming, **Release** sits beside *awaiting reply*.

### Answering where you are

Whatever the agent is stopped at appears under the chat, answerable in place:

- **A permission prompt**, with the rows Claude Code is actually showing — read off the screen, so the choice you click is the one that's pressed.
- **A question** (`AskUserQuestion`), single- or multi-select, with its options and a field to answer in your own words. Your answer goes in only while that question's own selector is on screen; if something else has replaced it, the answer is refused rather than typed into whatever is there now.
- **Claude's workspace-trust dialog** — *Trust this folder?* — which a resumed agent can stop at before it has a session at all.
- **A question on one of its tasks** — see [blockers](/features/tasks/#when-an-agent-stops-on-a-question).
- **A watch script it wants to run** — **Allow this watch script?**, with the whole script, **Allow** and **Don't allow**; see [watch scripts](/features/follow-ups/#you-approve-each-script-first).

A card ignores clicks in its first moments on screen, so a prompt that appears under your pointer as you click can't be answered by accident. It also holds still while it's up: it is redrawn only when the prompt actually changes, so typing an answer isn't interrupted.

### The composer

Type to the agent at the bottom. `Enter` sends and `Shift+Enter` adds a line; drafts are kept per chat. Paste a screenshot or Finder files, or drop files on the composer, and they're attached the same way as in the terminal's [composer dock](/features/terminal/#composer-dock) — for an agent on an SSH host, each file is copied over to the host first. Attachments are for Claude Code chats.

A sent message stays in the chat as a bubble marked **Sending…**, then **Queued** (Claude Code holds a message until its current turn ends) or **Delivered**, until the agent's transcript shows it. A queued message isn't in the transcript yet, so without the bubble it would look lost.

Nothing is sent while a prompt is open on the agent: text typed into a permission dialog lands in the dialog, and a digit picks a row. Answer the prompt first. An agent that has exited or isn't connected is woken before the message goes, rather than having it typed at a shell.

With Overlord on and a rule that applies, a **bolt** in the composer fires an Overlord rule at this agent by hand — what the Trigger menu on the old Fleet cards used to do.

**The agent's work** on the right lists the open tasks on that tab, and anything unclaimed in its workspace.

## Weave

The Weave draws the work as a picture: agents down the left, the open tasks grouped by workstream in the middle, and strings between them.

- **Moving dots** on a string mean the task is active.
- **A slow amber dash running backwards** means it's waiting.
- **A faint dotted string** joins a task to what it depends on.

Click an agent to fade everything it isn't part of. Workstreams with a question waiting on you come first, and a task that has claimed to be active or to-do for 14 days without changing is drawn quiet — month-old "active" rows used to look exactly like live work.

Select a task to see it on the right: lane, workstream, agent and age; its question with the controls to answer it, or the tasks it's waiting on, or — if neither — that no reason was recorded; then its description and log. **Talk to the agent** takes you to its chat in Focus.

## Decisions

Every question waiting on you, across the window, oldest first — decisions to make and actions only you can take — each answerable in place, with **Talk to the agent** and **Open the tab** beside it.

A [watch script](/features/follow-ups/#you-approve-each-script-first) an agent wants maiTerm to run for it waits here too, as **Allow this watch script?** with the whole script shown, and counts toward Decisions and toward what needs you. Unlike the rest of the Loom, these include scripts from suspended workspaces and archived tabs: the question is still waiting even though the tab isn't. The notification announcing a script opens this view.

Below the queue is **Blocked with no reason recorded**: tasks sitting in Blocked with no question on them and nothing they're waiting on, so whatever they stopped on is only in the agent's chat. **Ask for the reason** types one question at the agent, and its answer comes back here as a proper question.

:::note
The Loom reads the same [task board](/features/tasks/) your agents write to and the same transcripts [maiLink](/features/mailink/) shows on your phone. A question answered on the phone, in the task panel or in the Loom is answered once — whichever you get to first. The same goes for a watch script, which can also be allowed from the tab's follow-ups list.
:::
