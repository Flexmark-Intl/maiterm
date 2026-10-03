---
title: Follow-ups
description: An agent schedules a prompt back into its own tab — at a time, when a service comes up, when a task ends, or when a script it wrote says the thing it's waiting on has happened.
---

"Check the deploy in twenty minutes." "Look at CI once it has run again." "Import the export when it lands." An agent that has to come back to something has had no way to do it — it stops, and the follow-up happens only if you remember it.

A **follow-up** is a prompt an agent schedules back into **its own tab**, for later. maiTerm holds it, not the agent's process, so it survives the agent exiting and maiTerm restarting, and when it comes due it's delivered into the same session, with the same files, services and SSH hosts the agent already had. It works for every runtime.

## Turning it on

Follow-ups live under [Overlord](/features/overlord/), in **Preferences → Overlord → Enable follow-ups**. The switch is on by default but only counts while Overlord itself is on — so turning Overlord on turns follow-ups on with it, and from then on they're a switch of their own.

While follow-ups are off, agents aren't told the feature exists and don't get the tools. Follow-ups already scheduled stay on their tabs, visible and cancellable, and are delivered once you turn the feature back on.

## What an agent can wait for

| Trigger | Comes due |
|---------|-----------|
| A time | In so many minutes, or at a clock time — 1 minute to 7 days out |
| A watch script | When a script the agent wrote passes — see [Watch scripts](#watch-scripts) |
| A service is ready | The next time a [stack](/features/stack/) service in this project comes up |
| A service stopped | The next time a running stack service stops or crashes |
| A task is done | When a [task](/features/tasks/) ends — done, or dropped, and the follow-up says which |

Event triggers fire on the **next** change: asking to hear when a service comes up while it's already up is refused, with a note to act now instead. If the service or task is removed before it happens, the agent is told that instead. Event and script follow-ups wait at most 7 days.

Each tab can hold 10 pending follow-ups. A follow-up fires once; an agent that wants to keep watching schedules the next one when this one arrives.

| Tool | What it does |
|------|--------------|
| `createFollowUp` | Schedule a prompt back to this tab, with exactly one trigger |
| `listFollowUps` | This tab's pending follow-ups, including ones you added |
| `cancelFollowUp` | Cancel one of this tab's follow-ups |

An agent can only schedule into its own tab. Typing into *another* agent's tab is what Overlord's guarded `driveTab` is for, and a follow-up tool that could do it would hand that power to every agent.

## Watch scripts

When an agent is really waiting for a **condition** — a file to appear, a PR's checks to finish, new errors in a log — a time follow-up is the wrong tool: re-armed to check, it spends a whole turn on every look, usually to find nothing has changed. That's the recommended alternative agents are told about: a **watch script**.

The agent writes the condition as a short script. maiTerm runs it on a schedule, without waking the agent, and the follow-up comes due when it passes. While it waits, nothing is spent.

- **Exit `0`** — the condition is met. Whatever the script printed is delivered with the follow-up, so the agent learns *what* it found without spending its first turn looking.
- **Exit `1`** — not yet; run again later.
- **Anything else**, or a timeout — the script is broken. After three broken runs in a row the follow-up comes due anyway, telling the agent the script broke and why, rather than waiting out seven days on a typo.

A script runs every 15 seconds to once a day (every 60 seconds by default), for up to a minute a run. Each run starts in the tab's folder with your login `PATH` but **not** your environment — no agent credentials, no account tokens — and everything it starts is killed when it ends, so a watch script can never leave a process running. It can keep state between runs in a file maiTerm gives it, which is how "changed since last time" is written.

Watch scripts run on your computer, so they're refused in a tab running over SSH, where the condition would be on the other machine.

### You approve each script first

Every command an agent normally runs passes its own permission check at the moment it runs, while you can see it. A watch script runs later, unattended, as you — so **each new script asks you first**.

A script waiting for you is a question, so it's asked where your other questions are: a card titled **Allow this watch script?** in the [Loom's Decisions](/features/loom/#decisions), in the tab's task panel, and at the top of the tab's follow-ups list. It's the same card everywhere, and answering it in one place answers it in all of them. A notification — **Allow a watch script?** and the tab's name — announces it, and inside maiTerm that notice stays up until the script is answered anywhere, your phone included; click it to open Decisions. Several scripts waiting share one notice.

The card shows the script **exactly** as it will run — the whole thing, wrapped, never clipped — with its name and schedule above it, and below it the folder it runs in, how long a run may take, and its line and character count. **What the agent is told when it passes** opens to show the message it will be woken with. Then **Allow** or **Don't allow**. A card ignores clicks in its first moments on screen, and for a moment after anything above it moves it, so a card that slides under your pointer as you click can't be answered by accident.

**Don't allow** means it will not run, and the agent is told so — the follow-up comes due saying you declined the script, rather than vanishing and leaving the agent waiting on a check nobody is running.

A script containing anything the card can't show truthfully, like invisible characters or a look-alike space, is refused when the agent creates it.

Approval is for that script **in that folder**: change one character and it's a new script, and the same script in another folder is a new approval. An agent re-arming a script you've already approved isn't asked again.

**Preferences → Overlord → Run watch scripts without asking** lets them run without the card. It's off by default, it's only for agents you already let run commands unattended, and no agent can switch it on for itself. Turning it off again puts every script it let through back behind a card.

## How it's delivered

A follow-up is typed into the agent **between turns** — never mid-turn, never at a permission prompt, never over something you've typed into its input box — and only once.

It arrives framed as `⟦FOLLOW-UP⟧`, saying when it was scheduled and for when, so the agent reads it as its own earlier note rather than as you having just typed it. One delivered more than five minutes late says by how much, so the agent can tell "the deploy should be done by now" from "this was yesterday's deploy". One you added yourself says so.

**If the agent has exited**, maiTerm can restart it and then deliver: it types the agent's own resume command at an empty shell prompt and waits for the session to come up. That starts a session, and uses your plan, while you're away, so it has its own switch — **Preferences → Overlord → Restart the agent for a follow-up**, on by default. Off, the follow-up waits until you start the agent. It isn't done for a tab whose agent ran over SSH, for a session that isn't on this machine, or for a tab exempt from Overlord.

A suspended or archived tab holds its follow-ups — an archived one delivers them once it's restored. A follow-up that can't be delivered before it expires is kept on the list as **expired** rather than dropped silently, so an agent that set one and never heard back is distinguishable from one that never set it.

## Seeing and managing them

A tab holding follow-ups shows a **clock badge** beside its name: dim while they wait, accent once one is due and waiting for the agent, yellow when a watch script is waiting for you, faded while follow-ups are off. Hover it for what's next.

Click the badge, or right-click the tab and choose **Follow-ups…**, to see the list. A script waiting for you sits at the top as its approval card. Every other row leads with what it waits for — "in 12m", "waiting for service `web` to be ready", "due 3m ago" — then the prompt, who added it, and, for one that's due but not delivered, what's holding it. A watch script's code is folded under a line giving its schedule and how its last check went. **Send now** sends one early, through the same checks as the schedule (and restarts an exited agent whatever the preference: the click is the consent); **Remove** takes one off the list, expired ones included. **Schedule one yourself** opens a form to add your own: a prompt, in so many minutes.

:::note
From your phone, [maiLink](/features/mailink/) shows a waiting watch script as a card you can **Allow** or **Don't allow**, under the same rules as the desktop's. It doesn't list the rest of a tab's follow-ups.
:::
