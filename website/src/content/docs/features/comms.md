---
title: Chat Threads
description: Point an agent at a Mattermost support thread — by permalink or by @mentioning the bot in a channel it's watching — and it works the bug to resolution from a maiTerm tab, reading the whole conversation, fixing the issue in your repo, and posting the answer back, while you stay in control of what it can act on.
---

A bug report lands in a Mattermost thread. Normally you'd read it, switch to your editor, reproduce it, fix it, then come back and write up what you did. Chat Threads collapses that loop: an agent tab binds to the thread, reads the entire conversation as a bug report, investigates and fixes the issue **in that tab's repository**, and posts the resolution back to the thread — without you leaving maiTerm.

There are two ways a thread reaches an agent, and both end in the same flow:

- **Paste a permalink.** Run `/maiterm resolve <permalink>` in an agent tab to bind that one thread by hand.
- **Let the bot get summoned.** Turn a tab into a **monitoring tab** and it watches the channels you choose; when anyone `@mentions` the bot in a thread, maiTerm binds that thread to the tab and hands the agent the request — hands-off, no permalink to copy.

You configure a bot account once, and from then on any agent tab can pick up a thread. The agent works silently while it investigates, asks a single addressed question if it gets genuinely stuck, and posts a two-part resolution when it's done — plain language for the support person, technical detail for the devs. Crucially, **you stay in control of what it's allowed to act on**: only messages that `@mention` the bot reach the agent, only people you've listed can summon it, and each message is scoped by who sent it.

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

1. **Bind and announce.** The agent binds the tab to the thread and pulls in the full conversation. Because Mattermost only delivers a notification on an exact `@username`, its **first reply tells the humans how to reach it** — "`@mention` me to send me a message" — using the bot's real username.
2. **Investigate silently.** While it works, the agent stays quiet on the thread — no progress chatter. It reproduces and fixes the issue in the tab's repo.
3. **One question if blocked.** If it genuinely can't proceed without more information, it posts a single concise question, explicitly addressed to the right audience — **`@Support`** for what the customer saw or did, **`@Dev`** for questions about the codebase or release process — so the right person knows to answer.
4. **Post the resolution.** When the fix is verified, the agent posts it as a normal reply and asks the humans to test and confirm. The post has two parts: a short, jargon-free summary for the support person (what was wrong, what changes for the customer, and when), a `---` divider, then **Technical details (for devs)** — root cause, what changed, how it was verified.
5. **Stay bound until confirmed.** Posting a fix does **not** close the thread (see below).

Ambient discussion in the thread isn't pushed at the agent, but it can re-read the whole thread on demand at any point to catch up on messages that weren't addressed to it.

## Screenshots, both directions

Bug reports come with pictures. Images attached to a thread message are **staged where the agent can actually open them**: maiTerm downloads the attachment and hands the agent a real file path, so a screenshot of the broken screen becomes something it can look at rather than something it's told about. This works for an agent running on a remote host over SSH too — the image is pushed to the remote machine over the same bridge tunnel the rest of the integration uses.

The agent can attach images to its own replies as well — a before/after, an annotated screenshot, visual proof that a fix landed — and they're uploaded to Mattermost and posted with the reply like any other attachment. Remote agents can send images back the same way; maiTerm fetches the file from the remote host before uploading it.

Both directions cover the usual formats (PNG, JPEG, GIF, WebP), with sensible per-message size and count limits so a thread can't be used to shovel arbitrary files around.

## Watching channels and getting summoned

Copying a permalink for every report gets old. A **monitoring tab** removes that step: it watches the channels you pick and picks up threads on demand, so a bug report becomes a bound work item the moment someone asks for the bot.

Turn it on from the tab itself — **right-click the tab → Enable chat monitoring…**. A picker opens listing the channels the bot can watch, grouped by team. It only lists channels **the bot is a member of**, so you can't point it at a channel it can't actually read; add the bot to a channel in Mattermost first if you don't see it. Check the ones this tab should watch and confirm. The same right-click menu lets you edit that selection or disable monitoring later.

From then on, the tab is a **dispatcher**. Whenever someone `@mentions` the bot in a thread in one of those channels, maiTerm binds that thread to the tab and hands the agent the request with the full conversation already attached — no `/maiterm resolve`, no permalink. The agent then runs exactly the [end-to-end flow above](#working-a-thread-end-to-end), starting from its first reply.

- **One tab, several threads.** A monitoring tab works **up to three threads at once**. Each binding is independent — the agent keeps their investigations separate — and the tab's `@` indicator shows a live count of how many are bound. The indicator is **dim while the tab is monitoring but idle**, and turns **green with a count** once threads are bound.
- **Overflow queues.** A summon that arrives while the tab is already at its three-thread capacity, busy, or offline doesn't get dropped — it waits. When the tab is at capacity, the bot posts a **one-time reply** to the waiting thread ("I'm at capacity on other issues right now — I'll pick this up as soon as one closes out") so the humans aren't left wondering, and you get a notification. As soon as a thread closes out or the session comes back, the queued summon is picked up automatically.
- **Only a genuinely new ask summons.** Editing an old message never counts as one — fixing a typo in the root report of a thread you just closed out won't drag the whole thread back in as fresh work. Neither does a mention the bot has already replied to: confirming a fix can't re-open the thread it just closed. A real new `@mention` — "@bot it's broken again" — still summons as normal.

Only people you've authorized can summon the bot — see [Who can summon the bot](#who-can-summon-the-bot) below. An `@mention` from anyone else is never picked up; it just notifies you.

## You stay in control

The agent is working against a live customer channel, so the design keeps you — not the chat participants — in charge of what it can do.

### Only @mentions reach the agent

The thread keeps flowing normally, but **only messages that `@mention` the bot are delivered into the session**. Everything else stays ambient — the agent can read it for context, but it doesn't act on it. That means the agent responds to deliberate asks, not to every message in a busy channel.

### Who can summon the bot

Two lists under **Preferences → Integrations → Message Authority** decide both *who may summon a monitoring tab* and *how much authority a delivered message carries*. Both take one Mattermost username per line, and both are editable **only** in Preferences — no chat message can rewrite who the agent trusts.

There are three tiers:

- **Authorized operators** — usernames under **Authorized usernames**. They can summon the bot, and their `@mentions` carry your **full authority**; the agent treats them as if you'd typed them yourself.
- **Pickup users** — usernames under **Pickup users**. They can **summon** the bot too, but their work is treated as **investigate-and-report**, not full authority: the agent may reproduce, investigate (read-only), and reply, but it will **not** take destructive, irreversible, or scope-expanding actions on their say-so. Use this tier for people you trust to hand the bot a bug but not to authorize sweeping changes.
- **Everyone else** — anyone not on either list. Their `@mentions` **cannot summon** a monitoring tab at all; an attempt simply notifies you and is never picked up. When a thread is *already* bound (you resolved it by permalink, say), messages from these channel members are still delivered as context but are treated the same as a pickup user's — **information and requests only**, never a mandate for destructive or scope-expanding work.

So the two lists layer: authorized operators are the only tier that carries full authority; pickup users extend the *right to summon* to more people without extending that authority; everyone else can neither summon nor direct destructive work. If any delivered message asks for something out of scope ("can we just wipe all that?"), the agent relays it to you and waits for sign-off rather than doing it. Matching is by Mattermost username, so this is only as trustworthy as your server's identities.

### An operator kill switch

You can end a binding yourself at any time: right-click the tab and choose **End thread binding(s)**. On a tab working several threads at once this clears **all** of them; the agent can also close out a single thread on its own once a human confirms the fix. This is the human override — **severing a binding never depends on the agent cooperating**, and it posts nothing to the thread. Forwarding stops within a few seconds.

### A fix stays open until a human confirms

Posting a resolution no longer closes anything. The binding stays live, and the agent asks support to test and confirm:

- If someone replies that it's **still broken**, the agent keeps working — their messages keep arriving in the tab.
- If someone confirms it's **resolved**, the agent posts a brief sign-off and closes the thread out.

So a thread only closes on a human's confirmation, not on the agent's own belief that it's done.

### You're told when a reply can't be delivered

If someone `@mentions` the bot on a bound thread while its agent session isn't running, maiTerm doesn't silently swallow the message. It raises a notification — a toast or OS notification per your [notification mode](/features/agents/), deep-linking to the tab — so you know there's something waiting. The message isn't lost: the backlog is delivered as soon as you resume the session.

## Handing work to the right agent in a mesh

If the monitoring tab is part of a [Mesh Workspace](/features/mesh-workspace/), it doesn't have to work every thread itself. Before it digs in, it checks its peers: when an issue clearly belongs to another agent's repository — a peer whose purpose and working directory match the report — it hands that peer the investigation and fix, while **staying the dispatcher on the thread**.

The bound tab is still the only one connected to the thread, so it keeps ownership of the conversation: it relays the request to the peer (passing the sender's authority through verbatim, so a pickup user's request stays investigate-and-report on the far end too), receives the peer's findings, and posts the resolution back itself. The right specialist does the work in the right repo; the thread only ever hears from the one bot it summoned.

## Shaping how the agent writes

The **Response Instructions** field (Preferences → Integrations) is free-text guidance for how the agent communicates on threads — tone, formatting, what to include or leave out, when to post. It's handed to the agent whenever it picks up a thread, layered on top of the built-in defaults. Use it for house style, for example:

> Address the customer by name if the report includes it. Keep the support-facing summary under four sentences and free of jargon. Sign off as "— maiTerm bot".

Response Instructions govern **communication only**. The safety rules — what the agent may act on, and whose messages carry authority — are fixed and can't be changed here.

:::tip
Chat Threads pairs naturally with the rest of maiTerm's agent tooling. A thread can be worked by an agent that's also part of a [Mesh Workspace](/features/mesh-workspace/) or connected via an [Agent Bridge](/features/agent-bridge/) — and answered from your pocket over [maiLink](/features/mailink/) when a reply lands while you're away from your desk.
:::
