---
title: Overlord
description: An opt-in per-window supervisor for a fleet of agents — a deterministic rules engine, an operations deck, and an optional supervisor agent, with every directive held for your approval by default.
---

With a dozen agents running, what you lose isn't any single answer — it's track. Which tab is about to hit a compaction wall. Which one has been sitting at a permission prompt for twenty minutes. Which one quietly stopped being connected to maiTerm at all. And underneath that, the same supervision typed by hand over and over: *have that commit reviewed*, *update the docs before you compact*, *write down what you're doing*.

**Overlord** watches every agent tab in a window and acts on rules you write. It is **off until you switch it on**, in **Preferences → Overlord**.

## Two halves

**The engine** is deterministic and headless. It reads state maiTerm already tracks — context percentage, agent state, last turn, commits, the [task board](/features/tasks/) — evaluates your rules against it, and sends directives. No model sits in that loop, so it's predictable and costs nothing to run.

**The agent** is optional: a real Claude Code tab in the Overlord workspace that you talk to, woken for the exceptions the rules can't settle. The board on disk is the truth; the agent's transcript is scratch, so a restart or a compaction loses nothing.

## Rules

A rule is a **condition**, a set of **guards**, and a **sequence** of steps. The checkpoint rule that ships with it is the worked example: *when context reaches 55%, tell the agent to prepare for compaction by bringing its docs, memory, code comments and tasks up to date; when that turn ends, send `/compact`* — waiting for each step to genuinely land before sending the next, rather than firing both into a busy tab.

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
- **One directive at a time per tab**, so sequences stay coherent. A directive that asks something holds the tab until it's answered, or for 15 minutes; a slash command such as `/model`, `/effort` or `/compact` never answers, so it doesn't hold the tab at all. A tab that is mid-compaction counts as busy, so nothing is typed into it halfway through.
- **A rate ceiling per tab per hour**, and a required quiet period before anything is sent.
- **Your own keystrokes abort a running sequence.** If you start typing, the ritual stops. Keys you press to answer a permission prompt don't count — a step that needed your approval carries on once you've given it.

Guards are **human-only**. The supervisor agent can propose changes to a rule's wording, timing and scope, but not to the conditions under which it may fire at all.

### The three rules that ship

| Rule | What it does |
|------|--------------|
| **Checkpoint before compaction** | At ~55% context, have the agent update its docs, memory, code comments and [task board](/features/tasks/) to prepare for compaction, then compact — instead of hitting the auto-compact wall mid-thought. One prep step: an earlier version asked twice and cost a turn on "already done" |
| **Review after commit** | After a commit lands, nudge the agent to have non-trivial work reviewed by a subagent before moving on |
| **Keep a task list** | A tab doing sustained work with nothing on the [maiTerm board](/features/tasks/) gets nudged to record it |

They behave like [triggers](/features/triggers/): seeded on first run, individually toggleable, editable in place, hideable and restorable, and auto-updated with new versions of maiTerm until you edit one — at which point your wording is frozen and left alone. If a later version retires a default you had edited, your copy stays, as a rule of your own.

There used to be a fourth, **Re-bind a running agent**, which typed `/maiterm init` into any tab whose agent was running but not connected. Agents bind themselves now, from their first request, so maiTerm no longer volunteers it. The **Agent running but unbound** condition is still there for a rule of your own, and the deck still offers the re-bind as a one-click repair.

Every rule is scoped **Everywhere** in the window by default, or to specific workspaces. A workspace rule is *additional* unless you mark it as superseding the global one.

## It asks before it types

**Propose mode is on out of the box.** Every directive a rule produces is held on the deck as a proposal, showing the exact words it would send, until you approve it. Nothing is typed into a tab until you click. Turn it off — in Preferences, or with the **Propose first** switch on the deck itself — and rules fire on their own.

## The deck

With Overlord enabled, a **♔ Overlord** row appears above the workspace list in the sidebar, badged with how many things are waiting on you. It opens the deck, which has four views and a strip of readouts across the top: how many tabs are watched, the peak context in the window, how many sequences are in flight, how many things need you, and how many directives were sent in the last 24 hours.

![The Overlord deck showing the Board view — workstreams from four workspaces indexed down the left, and task cards laid out across the backlog, active and blocked lanes](/screenshots/overlord-board.webp)

- **Loom** — the home view, and where `Cmd+Shift+J` lands: work with each agent without going into its terminal. A condensed chat with a composer, its permission prompts and questions answered in place, its state, model, context and any sequence in flight in the chat header, and a picture of who is working on what. It replaced the old **Fleet** grid of agent cards. See [Workstream Loom](/features/loom/).
- **Triage** — one severity-ordered queue of everything wanting a person: proposals, escalations, permission prompts, context pressure, stale work, [work one agent handed to another tab](/features/tasks/#handing-work-to-another-tab), tabs that have stopped answering. Not six stacked lists. Every card carries its own remedy, and a **Run all** clears the two things that need no judgement — re-bind every unbound agent, then approve every pending proposal — paced so a deck of forty signals doesn't become forty simultaneous API streams. It reports what came back, not what it typed: a `/maiterm init` typed at a tab whose agent is gone is delivered perfectly and achieves nothing, so the run stays open until each target has either re-bound or run out of time, and says how many never answered.
- **Board** — the [task board](/features/tasks/) for the whole window, indexed by workstream rather than by workspace. Drag a card between lanes, or onto a workstream in the index to move it to that job; `Escape` cancels a drag mid-flight.
- **Ledger** — a verbatim record of every directive sent: which tab, which rule (or you, or the agent), the exact bytes, and what came of it. Because injections are by design indistinguishable from you typing, this is the only way to reconstruct who told a project to do something at 3am.

**You're never asked the same question twice.** Every question has exactly one asker. By default that's the tab: when it hits a decision only you can make, it asks you directly — on screen, where you can answer it — and that prompt is already on your board, in the [Loom](/features/loom/) and on your phone. So the supervisor agent never repeats it or relays it: its copy would land in a transcript you'd then have to leave anyway to answer the original. A card about such a tab carries **Open tab** and says why there's no second prompt. Everything the supervisor can actually *do* something about — a blocked tab, a timed-out step, an unanswered directive — still reaches it, because fixing one before you get to the board is most of the point of running a supervisor. If you'd rather the supervisor be the asker, see [letting the agent answer escalations](#letting-the-agent-answer-escalations).

**A directive nobody will answer can be let go.** A tab that owes an answer is out of reach — no rule fires at it and the supervisor can't drive it — until it replies or the 15 minutes run out. When you can see no reply is coming, **Release** stops the wait at once: it sits beside *awaiting reply* in the agent's chat header in the [Loom](/features/loom/), and on the unanswered-directive card in Triage. A reply that turns up afterwards is not collected, and the unanswered-directive card goes with the directive it described. A wait a rule is running as one of its own steps isn't yours to release — that sequence ends on its own timeout.

**A blocked card withdraws itself** when the tab it's about reports that it isn't blocked any more, rather than sitting on the deck until someone dismisses a problem that already went away.

Two buttons sit in the command bar: **Scan tabs**, which reads every running agent tab and populates the board from it (safe to repeat — nothing is typed into anything), and **Re-bind *N***, which appears when more than one tab's agent is running unbound.

The deck takes its colours from whichever [theme](/features/themes/) you're running, and goes still under reduced-motion.

### From your phone

Escalations and proposals mirror to [maiLink](/features/mailink/), so you can approve or dismiss a proposal, clear an escalation, fire a rule at a tab or drive one without being at your desk. A new escalation rings the doorbell. Because the engine lives in the window it supervises, an action sent to a sleeping desktop is reported as *sent, not confirmed* until the next snapshot shows it landed.

## Firing a rule by hand

A rule's condition decides when it fires *on its own*. It doesn't decide when **you** may fire it — sometimes you can see that a tab needs checkpointing now, at 30% context rather than 55%.

The **bolt** in an agent's composer in the [Loom](/features/loom/) lists every rule that could run at that tab — enabled ones first, disabled ones tagged `off` — and fires the one you pick, whatever its when-clause says. Scope still holds: a rule pinned to a workspace stays pinned, because its steps were written for that workspace. Only the *when* is set aside. A rule you've added but not yet written any steps for doesn't appear at all.

The same menu is available from the tab you're already in. With Overlord on, the terminal's [composer dock](/features/terminal/#composer-dock) grows a **bolt** — beside the collapsed handle, and in the actions row when it's open — so you can fire a rule at the tab in front of you without going to the deck. A tab that has never hosted an agent doesn't get one, so a plain shell never sprouts a button that types into `bash`.

A manual fire obeys the same guards as an automatic one, and when it can't happen it says why — under the Loom's composer, or as a toast from the terminal's — rather than failing silently. The common refusals: the tab has no live agent, another sequence already owns it, or it's sitting at a permission prompt — which looks ready but isn't, so the fire is declined up front instead of holding the tab's slot for the full wait typing nothing.

## Follow-ups

With Overlord on, agents can also schedule a prompt back into their own tab — at a time, when a service comes up, when a task ends, or when a script they wrote says the thing they're waiting on has happened. It has its own switches in **Preferences → Overlord**. See [Follow-ups](/features/follow-ups/).

## Taking a tab off the board

Not everything in a window wants supervising: a scratch session, a demo window, a tab you're driving yourself step by step.

- **A single tab** — right-click its tab and choose **Exempt from Overlord**. The same item reads **Supervise with Overlord** to put it back.
- **A whole workspace** — the eye-off button on its row in the sidebar. It stays visible while the exemption is on rather than appearing on hover, because an exemption you can't see is one you forget you set. While a workspace is exempt, the per-tab item inside it is disabled and says so.

Exempt means the engine cannot see the tab **at all**: no rule evaluates against it, no proposal is raised for it, no rule can be fired at it by hand from either composer, and the supervisor agent's own tools refuse to touch it. It also **releases what the engine was already holding** — a sequence mid-flight, an outstanding directive still reading the tab's replies, a queued proposal — because you reach for "exempt" exactly while one of those is happening.

Task rows stay on the [board](/features/tasks/): exemption is about supervision, not about the work. The flag rides with the tab through reload, duplicate, split and window duplication.

## The supervisor agent

Clicking the Overlord row creates the Overlord workspace if it doesn't exist yet. Start an agent in a tab there and it becomes the supervisor: it receives your ruleset as standing doctrine and gets tools of its own, on top of everything an ordinary agent tab has.

| Tool | What it does |
|------|--------------|
| `listEscalations` | Pull the queue of things the engine couldn't settle deterministically |
| `driveTab` | Inject a directive into another tab in this window, with your authority |
| `releaseDirective` | Stop waiting on a tab's answer to an earlier directive, so it can be driven again — the same thing as **Release** on the deck |
| `getTabPrompt` / `answerTabPrompt` | See what a tab is stopped at, and answer it |
| `proposeRuleChanges` | Propose rule edits, or entries for its [playbook](#letting-the-agent-answer-escalations), for you to approve or reject |
| `archiveTab` / `closeTab` / `deleteArchivedTab` | Put a finished session away — one tab or a list of them in a single call |
| `recoverTab` / `resumeTab` / `resumeWorkspace` | Get a tab responding again, whatever state it's in |

Supervised agents — every other agent tab in the window — get one tool in return, `replyToOverlord`, to report ready, acknowledge a finished directive, or escalate a problem. A decision only you can make is asked of you directly, unless you've [handed those to the supervisor](#letting-the-agent-answer-escalations).

A few things worth knowing about how it behaves:

- **It goes through the same door the rules do.** `driveTab` is one injection tool with identical guards for both callers; the agent gets no privileged path, cannot race a sequence a rule is already running, and everything it sends lands in the same ledger.
- **Cleanup arrives as a list, so it's sent as one.** Pointing the supervisor at a window full of finished sessions used to cost a model turn per tab. Archiving, closing and deleting take a list of tabs in one call — but batching is transport, not permission: every tab still goes through its own quiet-window check and its own ledger entry, a refusal stops that tab rather than the batch, and the reply is clean only when *every* row succeeded.
- **Refusals are structured and specific.** "The tab is at a permission prompt" and "a rule owns this tab right now" call for opposite responses, so the refusal names which and the agent is told not to retry the ones retrying can't fix.
- **It answers routine prompts, and leaves the rest to you.** Approvals in service of work already underway are its to make, guided by your playbook; anything destructive or irreversible, anything touching money, credentials, production or an external party, and any question about what you actually *want* is yours. It neither answers nor repeats one of those — the prompt is already waiting for you on the board, in the Loom and on your phone.
- **It reaches tabs the engine can't type into** — suspended, archived, in a suspended workspace, or on the far end of an SSH connection.
- **Rule changes always ask.** `driveTab` is pre-approved because its guards are mechanical; `proposeRuleChanges` always prompts you — playbook entries included — and guards aren't proposable at all.

### Letting the agent answer escalations

**Preferences → Overlord → Overlord agent answers escalations** hands the supervisor the asking. It's **off by default**, and only counts while Overlord is on.

Turned on, supervised tabs bring the decisions they'd have asked you about to the supervisor instead: they escalate and wait, rather than also prompting you. The supervisor answers each one with `driveTab`, and its card on the deck clears once it has. It asks you only when its playbook doesn't settle a consequential call — the one case where it may, because then nobody else is asking. A card stays answerable by you too: **Open tab** and answer it yourself.

**The playbook** is your standing answers, one per line — *Dependency installs and lockfile updates: approve*, *Never push to main without asking me*. Edit it in **Preferences → Overlord → Overlord playbook**, shown whenever Overlord is on; the supervisor follows it when it answers routine prompts in either mode. When it does have to ask you, it can propose your answer as a new entry, which reaches you through the same approval dialog as a rule change. Nothing is added without your approval, and neither the switch nor the playbook can be changed by an agent.

Flipping the switch re-primes the supervisor with the matching instructions. A supervised tab picks the change up when its session next starts.

## Scope

Overlord is **per window**. Each maiTerm window has its own engine, its own rules in effect, its own deck and its own ledger, because a window is the unit of attention — the set of agents you're actually watching.

:::note
Overlord builds on the same [agent integration](/features/agents/) pipeline as the rest of maiTerm, and reads the same [task board](/features/tasks/) your agents write to. The supervisor agent is Claude Code; the agents it supervises can be any supported runtime.
:::
