---
title: Overlord
description: An opt-in per-window supervisor for a fleet of agents — a deterministic rules engine, an operations deck, and an optional supervisor agent, with every directive held for your approval by default.
---

With a dozen agents running, what you lose isn't any single answer — it's track. Which tab is about to hit a compaction wall. Which one has been sitting at a permission prompt for twenty minutes. Which one quietly stopped being connected to maiTerm at all. And underneath that, the same supervision typed by hand over and over: *have that commit reviewed*, *update the docs before you compact*, *write down what you're doing*.

**Overlord** watches every agent tab in a window and acts on rules you write. It is **off until you switch it on**, in **Preferences → Overlord**.

## Two halves

**The engine** is deterministic and headless. It reads state maiTerm already tracks — context percentage, agent state, last turn, commits, the [task list](/features/tasks/) — evaluates your rules against it, and sends directives. No model sits in that loop, so it's predictable and costs nothing to run.

**The agent** is optional: a real Claude Code tab in the Overlord workspace that you talk to, woken for the exceptions the rules can't settle. The board on disk is the truth; the agent's transcript is scratch, so a restart or a compaction loses nothing.

## Rules

A rule is a **condition**, a set of **guards**, and a **sequence** of steps. The checkpoint rule that ships with it is the worked example: *when context reaches 55%, tell the agent to bring its docs, memory, code comments and tasks up to date; when that turn ends, tell it to prepare for compaction; when that turn ends, send `/compact`* — waiting for each step to genuinely land before sending the next, rather than firing three directives into a busy tab.

Conditions fire on **semantic state**, not on terminal output (that's what [triggers](/features/triggers/) are for):

| Condition | Fires when |
|-----------|-----------|
| Context reaches *N*% | The tab's context window crosses a threshold |
| A commit lands | A `git commit` in that tab actually succeeded — a denied prompt, a refusing pre-commit hook and "nothing to commit" don't count |
| Every turn ends | The agent goes from working to idle |
| Tab goes idle for *N* min | Nothing has happened in the tab for a while |
| Work is not on the task list | The tab is doing sustained work with nothing recorded |
| Board task stale for *N* days | A task hasn't moved (parked tasks are exempt) |
| Agent running but unbound | An agent process is alive but isn't connected to maiTerm |
| Permission waits for *N* min | The tab is stopped at a permission prompt |
| Directive unacked for *N* min | An earlier directive was never answered |

Each **step** is either a free-text directive or a slash command, and can wait for something before the next one is sent — the turn to end, an acknowledgement from the agent, the context to drop below a percentage, or a period of quiet — with a timeout and a choice of what to do when it expires. A slash step declares which runtimes it's valid for and is skipped (and recorded) elsewhere, because a `/compact` injected into a runtime that doesn't know it lands as garbage in a live session.

### Guards, and why they're yours alone

Overlord types with **your** authority and no envelope wrapped around it — the target agent cannot tell an Overlord directive from you typing. That's deliberate: an envelope would teach agents to hold work "for the human", and you can't wrap a slash command in one anyway. So the safety is mechanical rather than a matter of prompt adherence:

- **A live agent must be there.** The real hazard isn't a misbehaving agent, it's an absent one — "spin up a review of that commit" typed at a bare shell prompt is a shell command.
- **One directive at a time per tab**, so sequences stay coherent.
- **A rate ceiling per tab per hour**, and a required quiet period before anything is sent.
- **Your own keystrokes abort a running sequence.** If you start typing, the ritual stops.

Guards are **human-only**. The supervisor agent can propose changes to a rule's wording, timing and scope, but not to the conditions under which it may fire at all.

### The four rules that ship

| Rule | What it does |
|------|--------------|
| **Checkpoint before compaction** | At ~55% context, have the agent update its docs, memory, code comments and [task list](/features/tasks/), prepare for compaction, then compact — instead of hitting the auto-compact wall mid-thought |
| **Review after commit** | After a commit lands, nudge the agent to have non-trivial work reviewed by a subagent before moving on |
| **Re-bind a running agent** | A tab whose agent is running but not connected to maiTerm gets a `/maiterm init`, restoring its tools and hooks. Only fires when the agent process is confirmed alive — a tab sitting at a shell is left alone |
| **Keep a task list** | A tab doing sustained work with nothing on the [maiTerm task list](/features/tasks/) gets nudged to record it |

They behave like [triggers](/features/triggers/): seeded on first run, individually toggleable, editable in place, hideable and restorable, and auto-updated with new versions of maiTerm until you edit one — at which point your wording is frozen and left alone.

Every rule is scoped **Everywhere** in the window by default, or to specific workspaces. A workspace rule is *additional* unless you mark it as superseding the global one.

## It asks before it types

**Propose mode is on out of the box.** Every directive a rule produces is held on the deck as a proposal, showing the exact words it would send, until you approve it. Nothing is typed into a tab until you click. Turn it off — in Preferences, or with the **Propose first** switch on the deck itself — and rules fire on their own.

## The deck

With Overlord enabled, a **♔ Overlord** row appears above the workspace list in the sidebar, badged with how many things are waiting on you. It opens the deck, which has four views and a strip of readouts across the top: how many tabs are watched, the peak context in the window, how many sequences are in flight, how many things need you, and how many directives were sent in the last 24 hours.

![The Overlord deck showing the Board view — workstreams from four workspaces indexed down the left, and task cards laid out across the backlog, active and blocked lanes](/screenshots/overlord-board.webp)

- **Triage** — one severity-ordered queue of everything wanting a person: proposals, escalations, permission prompts, context pressure, stale work, tabs that have stopped answering. Not six stacked lists. Every card carries its own remedy, and a **Run all** clears the two things that need no judgement — re-bind every unbound agent, then approve every pending proposal — paced so a deck of forty signals doesn't become forty simultaneous API streams. It reports what came back, not what it typed: a `/maiterm init` typed at a tab whose agent is gone is delivered perfectly and achieves nothing, so the run stays open until each target has either re-bound or run out of time, and says how many never answered.
- **Fleet** — a card per agent tab: context ring, live state, how long since its last turn, what it's working on, and any sequence in flight with its step progress. Sort the grid by **peak context** (most in need first) or by **latest activity**. **View** navigates to the tab and **Trigger ▾** fires a rule at it by hand. Tabs inside a suspended workspace get no card each — a dozen rows describing agents nobody expects to be running — and the bar above the grid counts them so you can see where they went; a tab suspended on its own inside a live workspace is still shown.
- **Board** — the [task board](/features/tasks/) for the whole window, indexed by workstream rather than by workspace, with cards you drag between lanes.
- **Ledger** — a verbatim record of every directive sent: which tab, which rule (or you, or the agent), the exact bytes, and what came of it. Because injections are by design indistinguishable from you typing, this is the only way to reconstruct who told a project to do something at 3am.

Two buttons sit in the command bar: **Scan tabs**, which reads every running agent tab and populates the board from it (safe to repeat — nothing is typed into anything), and **Re-bind *N***, which appears when more than one tab's agent is running unbound.

The deck takes its colours from whichever [theme](/features/themes/) you're running, and goes still under reduced-motion.

### From your phone

Escalations and proposals mirror to [maiLink](/features/mailink/), so you can approve or dismiss a proposal, clear an escalation, fire a rule at a tab or drive one without being at your desk. A new escalation rings the doorbell. Because the engine lives in the window it supervises, an action sent to a sleeping desktop is reported as *sent, not confirmed* until the next snapshot shows it landed.

## Firing a rule by hand

A rule's condition decides when it fires *on its own*. It doesn't decide when **you** may fire it — sometimes you can see that a tab needs checkpointing now, at 30% context rather than 55%.

**Trigger ▾** on any fleet card lists every rule that could run at that tab — enabled ones first, disabled ones tagged `off` — and fires the one you pick, whatever its when-clause says. Scope still holds: a rule pinned to a workspace stays pinned, because its steps were written for that workspace. Only the *when* is set aside. A rule you've added but not yet written any steps for doesn't appear at all.

The same menu is available from the tab you're already in. With Overlord on, the [composer dock](/features/terminal/#composer-dock) grows a **bolt** — beside the collapsed handle, and in the actions row when it's open — so you can fire a rule at the tab in front of you without going to the deck. A tab that has never hosted an agent doesn't get one, so a plain shell never sprouts a button that types into `bash`.

A manual fire obeys the same guards as an automatic one, and when it can't happen it says why on the card you clicked (or as a toast, from the composer) rather than as a note at the top of a grid that doesn't say which card it means. The common refusals: the tab has no live agent, another sequence already owns it, or it's sitting at a permission prompt — which looks ready but isn't, so the fire is declined up front instead of holding the tab's slot for the full wait typing nothing.

## Taking a tab off the board

Not everything in a window wants supervising: a scratch session, a demo window, a tab you're driving yourself step by step.

- **A single tab** — right-click its tab and choose **Exempt from Overlord**. The same item reads **Supervise with Overlord** to put it back.
- **A whole workspace** — the eye-off button on its row in the sidebar. It stays visible while the exemption is on rather than appearing on hover, because an exemption you can't see is one you forget you set. While a workspace is exempt, the per-tab item inside it is disabled and says so.

Exempt means the engine cannot see the tab **at all**: no rule evaluates against it, no proposal is raised for it, it gets no fleet card, no Trigger menu and no bolt in its composer, and the supervisor agent's own tools refuse to touch it. It also **releases what the engine was already holding** — a sequence mid-flight, an outstanding directive still reading the tab's replies, a queued proposal — because you reach for "exempt" exactly while one of those is happening.

Task rows stay on the [board](/features/tasks/): exemption is about supervision, not about the work. The flag rides with the tab through reload, duplicate, split and window duplication.

## The supervisor agent

Clicking the Overlord row creates the Overlord workspace if it doesn't exist yet. Start an agent in a tab there and it becomes the supervisor: it receives your ruleset as standing doctrine and gets tools of its own, on top of everything an ordinary agent tab has.

| Tool | What it does |
|------|--------------|
| `listEscalations` | Pull the queue of things the engine couldn't settle deterministically |
| `driveTab` | Inject a directive into another tab in this window, with your authority |
| `getTabPrompt` / `answerTabPrompt` | See what a tab is stopped at, and answer it |
| `proposeRuleChanges` | Propose rule edits for you to approve or reject |
| `archiveTab` / `closeTab` / `deleteArchivedTab` | Put a finished session away |
| `recoverTab` / `resumeTab` / `resumeWorkspace` | Get a tab responding again, whatever state it's in |

Supervised agents — every other agent tab in the window — get one tool in return, `replyToOverlord`, to report ready, acknowledge a finished directive, or escalate something that needs a human.

A few things worth knowing about how it behaves:

- **It goes through the same door the rules do.** `driveTab` is one injection tool with identical guards for both callers; the agent gets no privileged path, cannot race a sequence a rule is already running, and everything it sends lands in the same ledger.
- **Refusals are structured and specific.** "The tab is at a permission prompt" and "a rule owns this tab right now" call for opposite responses, so the refusal names which and the agent is told not to retry the ones retrying can't fix.
- **It answers routine prompts, and escalates the rest.** Approvals in service of work already underway are its to make; anything destructive or irreversible, anything touching money, credentials, production or an external party, and any question about what you actually *want* goes to you instead.
- **It reaches tabs the engine can't type into** — suspended, archived, in a suspended workspace, or on the far end of an SSH connection.
- **Rule changes always ask.** `driveTab` is pre-approved because its guards are mechanical; `proposeRuleChanges` always prompts you, and guards aren't proposable at all.

## Scope

Overlord is **per window**. Each maiTerm window has its own engine, its own rules in effect, its own deck and its own ledger, because a window is the unit of attention — the set of agents you're actually watching.

:::note
Overlord builds on the same [agent integration](/features/agents/) pipeline as the rest of maiTerm, and reads the same [task list](/features/tasks/) your agents write to. The supervisor agent is Claude Code; the agents it supervises can be any supported runtime.
:::
