---
title: Chat Threads
description: Point an agent at a Mattermost support thread — by permalink or by @mentioning the bot in a channel it's watching — and it works the bug to resolution from a maiTerm tab, reading the whole conversation, fixing the issue in your repo, and posting the answer back, while you stay in control of what it can act on.
---

A bug report lands in a Mattermost thread. Normally you'd read it, switch to your editor, reproduce it, fix it, then come back and write up what you did. Chat Threads collapses that loop: an agent tab binds to the thread, reads the entire conversation as a bug report, investigates and fixes the issue **in that tab's repository**, and posts the resolution back to the thread — without you leaving maiTerm.

There are three ways a tab ends up working a thread, and they all end in the same flow:

- **Paste a permalink.** Run `/maiterm resolve <permalink>` in an agent tab to bind that one thread by hand.
- **Let the bot get summoned.** Turn a tab into a **monitoring tab** and it watches the channels you choose; when anyone `@mentions` the bot in a thread, maiTerm binds that thread to the tab and hands the agent the request — hands-off, no permalink to copy.
- **Let the agent raise it.** A monitoring tab can also **open** a thread — an incident it found, a heads-up, a question it needs a human to answer — and stay bound to it for the replies. See [Raising a thread of its own](#raising-a-thread-of-its-own).

You configure a bot account once, and from then on any agent tab can pick up a thread. The agent acknowledges the thread the moment it takes it, then works silently while it investigates, asks a single addressed question if it gets genuinely stuck, and posts a two-part resolution when it's done — plain language for the support person, technical detail for the devs. Crucially, **you stay in control of what it's allowed to act on**: on a thread someone else started, only messages that `@mention` the bot reach the agent; only people you've listed can summon it; and each message is scoped by who sent it.

:::note
Chat Threads is part of maiTerm's [agent integration](/features/agents/). It needs a supported agent (Claude Code) running in the tab, works over SSH through the same MCP bridge as the rest of the integration, and reaches you through maiTerm's existing [notifications](/features/agents/) — including a ring on [maiLink](/features/mailink/) when a reply arrives and no session is live to take it.
:::

## What it's for

The shape of the work is a support or QA channel where someone relays a customer's bug, and a developer picks it up. Chat Threads is the developer's side of that hand-off:

- **A support thread as a work item.** The whole conversation — the root report plus the back-and-forth — comes into the tab as a transcript, so the agent starts with the full context, not a one-line summary.
- **A fix in the actual repo.** The agent works in the tab's working directory, so it reproduces and fixes against real code, not a description of it.
- **The answer, back where it was asked.** The resolution is posted to the same thread, addressed to the people who need it, so support and the customer hear back in the place they reported it.

A thread binds to a single tab, but a tab can work **up to three threads at once** (see [Watching channels](#watching-channels-and-getting-summoned) below). A tab with a live binding shows a green `@` indicator in the tab bar with a count; hover it for the binding details, or right-click for the controls below.

## Setting it up

Everything lives in **Preferences → Integrations**. You need a **bot account** on your Mattermost server, and the bot has to be a member of any channel it should read or post in.

1. **Provider** — Mattermost. (The setting is a seam for other chat platforms later; Mattermost is what ships today.)
2. **Server URL** — the base URL of your Mattermost server, e.g. `https://chat.example.com`.
3. **Bot Token** — the access token of a Mattermost bot account (create one under **System Console → Integrations → Bot Accounts**). The token is stored locally and is **never exposed to agents** — no chat message and no MCP call can read it back.
4. Click **Test Connection**. On success maiTerm confirms the bot account it authenticated as (`Connected — bot account @yourbot`), so you know the token and URL are right before you rely on them.

Two more blocks on the same screen shape how the agent behaves — both optional, both covered below: **Message Authority** (who the agent trusts, and who may summon it) and **Response Instructions** (how the agent writes).

## Working a thread, end to end

From an agent tab whose working directory is the relevant repository, run:

```
/maiterm resolve <mattermost-permalink>
```

Get the permalink from Mattermost's **⋯ → Copy Link** on the message. From there the agent runs the flow itself:

1. **Bind and acknowledge.** The agent binds the tab to the thread and pulls in the full conversation, then — **before it investigates anything** — posts a short acknowledgement: that it's picked the thread up, its one-line read of what's being asked, and a rough sense of what happens next. The read-back matters: a misunderstanding surfaces immediately instead of ten minutes later in the shape of the wrong fix. Because Mattermost only delivers a notification on an exact `@username`, that first reply also **tells the humans how to reach it** — "`@mention` me to send me a message" — using the bot's real username. Handing the work off to a subagent or a [mesh](/features/mesh-workspace/) peer doesn't stand in for the ack, and doesn't happen before it.
2. **Investigate silently.** After the ack, the agent stays quiet on the thread — no progress chatter. It reproduces and fixes the issue in the tab's repo.
3. **One question if blocked.** If it genuinely can't proceed without more information, it posts a single concise question, explicitly addressed to the right audience — **`@Support`** for what the customer saw or did, **`@Dev`** for questions about the codebase or release process — so the right person knows to answer.
4. **Post the resolution.** When the fix is verified, the agent posts it as a normal reply and asks the humans to test and confirm. The post has two parts: a short, jargon-free summary for the support person (what was wrong, what changes for the customer, and when) ending with the ask — an `@mention` of whoever should verify it — then a `---` divider and **Technical details (for devs)**: root cause, what changed, how it was verified. Anything asked of a human sits above the divider, and the technical detail is always the last thing in the post, so the call to action is never buried under a wall of developer detail.
5. **Post and release.** Posting the resolution also releases the binding, freeing the tab's slot — and the post tells the humans how to come back: an `@mention` on the thread summons the agent straight back with the whole conversation. On a thread it *can't* be summoned back to, it stays bound instead (see [below](#a-finished-thread-releases-its-slot)).

Ambient discussion in the thread isn't pushed at the agent, but it can re-read the whole thread on demand at any point to catch up on messages that weren't addressed to it.

## Screenshots, both directions

Bug reports come with pictures. Images attached to a thread message are **staged where the agent can actually open them**: maiTerm downloads the attachment and hands the agent a real file path, so a screenshot of the broken screen becomes something it can look at rather than something it's told about. This works for an agent running on a remote host over SSH too — the image is pushed to the remote machine over the same bridge tunnel the rest of the integration uses.

A reply that is *nothing but* an image counts as a message. Mattermost splits a drag-and-drop upload from the text that introduced it, so the screenshot arrives as a post with an empty body; it's still delivered, and on a mention-gated thread it rides in on the `@mention` its author posted alongside it moments earlier.

The agent can attach images to its own replies as well — a before/after, an annotated screenshot, visual proof that a fix landed — and they're uploaded to Mattermost and posted with the reply like any other attachment. Remote agents can send images back the same way; maiTerm fetches the file from the remote host before uploading it.

Both directions cover the usual formats (PNG, JPEG, GIF, WebP), with sensible per-message size and count limits so a thread can't be used to shovel arbitrary files around.

## Watching channels and getting summoned

Copying a permalink for every report gets old. A **monitoring tab** removes that step: it watches the channels you pick and picks up threads on demand, so a bug report becomes a bound work item the moment someone asks for the bot.

Turn it on from the tab itself — **right-click the tab → Enable chat monitoring…**. A picker opens listing the channels the bot can watch, grouped by team. It only lists channels **the bot is a member of**, so you can't point it at a channel it can't actually read; add the bot to a channel in Mattermost first if you don't see it. Check the ones this tab should watch and confirm. The same right-click menu lets you edit that selection or disable monitoring later.

From then on, the tab is a **dispatcher**. Whenever someone `@mentions` the bot in a thread in one of those channels, maiTerm binds that thread to the tab and hands the agent the request with the full conversation already attached — no `/maiterm resolve`, no permalink. The agent then runs exactly the [end-to-end flow above](#working-a-thread-end-to-end), starting from its first reply.

- **One tab, several threads.** A monitoring tab works **up to three threads at once**. Each binding is independent — the agent keeps their investigations separate — and the tab's `@` indicator shows a live count of how many are bound. The indicator is **dim while the tab is monitoring but idle**, and turns **green with a count** once threads are bound.
- **Overflow queues.** A summon that arrives while the tab is already at its three-thread capacity, has no agent session, or has no terminal running doesn't get dropped — it waits, and the notification names which of those it was, because they don't clear the same way: a capacity hold only drains when one of the bound threads is released, while the others clear the moment the tab is back. When the tab is at capacity, the bot also posts a **one-time reply** to the waiting thread ("I'm at capacity on other issues right now — I'll pick this up as soon as one closes out") so the humans aren't left wondering. As soon as a thread is released or the session comes back, the queued summon is picked up automatically — and if several slots free at once, the waiting threads are handed over **one per pass** rather than typed into the tab back to back, so none of them is lost.
- **Only a genuinely new ask summons.** Editing an old message never counts as one — fixing a typo in the root report of a thread you just closed out won't drag the whole thread back in as fresh work. Neither does a mention the bot has already replied to: confirming a fix can't re-open the thread it just closed. A real new `@mention` — "@bot it's broken again" — still summons as normal.

Only people you've authorized can summon the bot — see [Who can summon the bot](#who-can-summon-the-bot) below. An `@mention` from anyone else is never picked up; it just notifies you.

## Raising a thread of its own

Answering is only half a conversation. An agent on a monitoring tab can also **open** a thread: a regression it tripped over while working on something else, a heads-up that a deploy is about to change behaviour, a question only a human can settle. It posts a new root message in one of the channels that tab monitors and binds itself to it, so the answers come back to the same tab.

Ask for one with `/maiterm raise <what to post>`, or let the agent decide it needs to — the rules are the same either way:

- **Only channels you put on that tab's monitor list.** An agent can't post into a channel it happened to discover; the monitored set is the whole of its reach, and you set it from the tab's right-click menu. If the tab monitors more than one, the agent names which.
- **It counts against the three-thread cap.** A thread the agent opened is a live thread it owns — it works it to resolution and then releases it like a summoned one, and an `@mention` brings it back.
- **Every reply comes back.** Nobody should have to `@mention` a bot they didn't summon, so while the agent holds a thread it opened, the mention gate is off and all human replies are delivered. Summoned and permalink-bound threads stay mention-gated as before — and so does a thread the agent has released, since the gate is a property of the binding.
- **It has to `@mention` whoever should see it.** A new root post notifies nobody by itself, so the opening message names the people it's for by their exact Mattermost username.

An agent can also post a genuine fire-and-forget notice — a heads-up it expects no answer to — without binding the thread at all.

Authority is unchanged on a thread the agent opened: an authorized operator's reply still carries your full authority, and everyone else's still buys investigation and answers but not changes. See below.

## You stay in control

The agent is working against a live customer channel, so the design keeps you — not the chat participants — in charge of what it can do.

### Only @mentions reach the agent

The thread keeps flowing normally, but **only messages that `@mention` the bot are delivered into the session**. Everything else stays ambient — the agent can read it for context, but it doesn't act on it. That means the agent responds to deliberate asks, not to every message in a busy channel.

The one exception is a thread the agent [opened itself](#raising-a-thread-of-its-own): it asked the question, so every reply is delivered without anyone having to mention it. Threads that reached it by summon or by permalink — which is to say, every thread a human started — stay mention-gated.

### Who can summon the bot

Two lists under **Preferences → Integrations → Message Authority** decide both *who may summon a monitoring tab* and *how much authority a delivered message carries*. Both take one Mattermost username per line, and both are editable **only** in Preferences — no chat message can rewrite who the agent trusts.

There are three tiers:

- **Authorized operators** — usernames under **Authorized usernames**. They can summon the bot, and their `@mentions` carry your **full authority**; the agent treats them as if you'd typed them yourself.
- **Pickup users** — usernames under **Pickup users**. They can **summon** the bot too, but their messages don't carry full authority. Use this tier for people you trust to hand the bot a bug but not to authorize sweeping changes.
- **Everyone else** — anyone not on either list. Their `@mentions` **cannot summon** a monitoring tab at all; an attempt simply notifies you and is never picked up. When a thread is *already* bound (you resolved it by permalink, say), messages from these channel members are still delivered as context and are treated the same as a pickup user's.

For everyone below the authorized tier, the line the agent draws is **read versus change**:

- **Reading is free.** Investigating, reading the code, explaining how something works, reproducing the report, confirming a bug is real, answering questions, replying on the thread — the agent does all of that on a support person's say-so without asking anyone. That is what support and pickup users are there for, and making them wait for a sign-off to get a question answered would defeat the point.
- **Changing needs a go-ahead.** Editing code, committing, deploying, running a migration, deleting or resetting data, changing configuration, or working beyond the reported issue — none of that happens on a non-authorized request. The agent replies on the thread `@mentioning` one of your authorized operators, stating what's being asked and what it would do, and waits — and it can raise a notification to you at the terminal as well.

So "can you check whether X is broken?" gets an answer; "can you fix X" — or "can we just wipe all that?" — gets a question put to someone you trust. The two lists layer accordingly: authorized operators are the only tier that carries full authority; pickup users extend the *right to summon* to more people without extending that authority; everyone else can neither summon nor direct a change. The agent is told who your authorized operators are so it knows whom to ask — read-only, like the lists themselves. Matching is by Mattermost username, so this is only as trustworthy as your server's identities.

### An operator kill switch

You can end a binding yourself at any time: right-click the tab and choose **End thread binding(s)**. On a tab working several threads at once this clears **all** of them; the agent releases individual threads on its own as it finishes them. This is the human override — **severing a binding never depends on the agent cooperating**, and it posts nothing to the thread. Forwarding stops within a few seconds.

### A finished thread releases its slot

Waiting for a human to confirm a fix can take hours, and a tab has only three slots — a finished thread left bound backs up every new report behind it. So posting the resolution and **releasing the binding** is the normal ending: the agent asks support to test and confirm, and lets go of the thread while they do.

Releasing isn't abandoning. The thread stays live for the humans, and an `@mention` of the bot on it **summons the agent straight back with the full conversation**, including everything said while it was away:

- If someone replies that it's **still broken** — `@bot it's still doing it` — the thread comes back to the tab as a fresh pickup and the agent keeps working, with all the earlier context.
- The agent stays bound only while it's **actively working** the thread, **waiting on a question it asked**, or on a thread it **can't be summoned back to**.

That last case is the safeguard, and it's why releasing is safe to do at all: an `@mention` can only re-summon on a channel that tab **monitors**. A thread bound by permalink in a channel the tab doesn't monitor is a one-way door — nothing could bring the agent back — so there it holds the binding until the humans genuinely have what they need. The agent is told which case it's in when it picks the thread up, rather than guessing.

Because a released thread is no longer bound, it's also back to being mention-gated: a follow-up has to `@mention` the bot to reach the agent. You can still end any binding yourself at any time (above).

### A thread message never answers a prompt for you

If a reply arrives while the agent is showing you something to decide — a multiple-choice question or a permission prompt — it isn't delivered yet. Those prompts are selection UIs, not text boxes, so a message typed into one would pick an option on your behalf and then vanish, taking both your answer and the message with it. maiTerm holds the message instead and delivers it the moment you've answered. These holds are silent: they clear in seconds, so they don't raise a notification or a reply on the thread the way a capacity or offline hold does.

### You're told when a reply can't be delivered

If someone `@mentions` the bot on a bound thread while its agent session isn't running, maiTerm doesn't silently swallow the message. It raises a notification — a toast or OS notification per your [notification mode](/features/agents/), deep-linking to the tab — so you know there's something waiting. The message isn't lost: the backlog is delivered as soon as you resume the session.

## Handing work to the right agent in a mesh

If the monitoring tab is part of a [Mesh Workspace](/features/mesh-workspace/), it doesn't have to work every thread itself. Before it digs in, it checks its peers: when an issue clearly belongs to another agent's repository — a peer whose purpose and working directory match the report — it hands that peer the investigation and fix, while **staying the dispatcher on the thread**.

The bound tab is still the only one connected to the thread, so it keeps ownership of the conversation: it relays the request to the peer, receives the peer's findings, and posts the resolution back itself. The right specialist does the work in the right repo; the thread only ever hears from the one bot it summoned.

**Delegating never widens what may be done.** Only the bound tab receives a message's authority tag, so a delegate — a mesh peer, or a subagent the dispatcher spins up to work a thread in its own repo — is told the sender's tier *and handed the authority rule word for word*, quoted from the single place it's written down rather than summarized. That matters because the read-versus-change line is easy to summarize permissively: an ordinary bug fix isn't destructive or irreversible, so a loose restatement reads as "go ahead". A support-tier request gets the same answer whether it's worked in the bound tab or one repo over, and a change a delegate wants to make still needs an authorized operator's go-ahead, asked for through the dispatcher.

## Shaping how the agent writes

The **Response Instructions** field (Preferences → Integrations) is free-text guidance for how the agent communicates on threads — tone, formatting, what to include or leave out, when to post. It's handed to the agent whenever it picks up a thread, layered on top of the built-in defaults. Use it for house style, for example:

> Address the customer by name if the report includes it. Keep the support-facing summary under four sentences and free of jargon. Sign off as "— maiTerm bot".

Response Instructions govern **communication only**. The safety rules — what the agent may act on, and whose messages carry authority — are fixed and can't be changed here.

:::tip
Chat Threads pairs naturally with the rest of maiTerm's agent tooling. A thread can be worked by an agent that's also part of a [Mesh Workspace](/features/mesh-workspace/) or connected via an [Agent Bridge](/features/agent-bridge/) — and answered from your pocket over [maiLink](/features/mailink/) when a reply lands while you're away from your desk.
:::
