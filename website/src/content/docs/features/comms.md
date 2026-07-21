---
title: Chat Threads
description: Point an agent at a Mattermost support thread and it works the bug to resolution from a maiTerm tab — reading the whole conversation, fixing the issue in your repo, and posting the answer back — while you stay in control of what it can act on.
---

A bug report lands in a Mattermost thread. Normally you'd read it, switch to your editor, reproduce it, fix it, then come back and write up what you did. Chat Threads collapses that loop: paste the thread's permalink into an agent tab with `/maiterm resolve`, and the agent binds that tab to the thread. It reads the entire conversation as a bug report, investigates and fixes the issue **in that tab's repository**, and posts the resolution back to the thread — without you leaving maiTerm.

You configure a bot account once, and from then on any agent tab can pick up a thread. The agent works silently while it investigates, asks a single addressed question if it gets genuinely stuck, and posts a two-part resolution when it's done — plain language for the support person, technical detail for the devs. Crucially, **you stay in control of what it's allowed to act on**: only messages that `@mention` the bot reach the agent, and each one is scoped by who sent it.

:::note
Chat Threads is part of maiTerm's [agent integration](/features/agents/). It needs a supported agent (Claude Code) running in the tab, works over SSH through the same MCP bridge as the rest of the integration, and reaches you through maiTerm's existing [notifications](/features/agents/) — including a ring on [maiLink](/features/mailink/) when a reply arrives and no session is live to take it.
:::

## What it's for

The shape of the work is a support or QA channel where someone relays a customer's bug, and a developer picks it up. Chat Threads is the developer's side of that hand-off:

- **A support thread as a work item.** The whole conversation — the root report plus the back-and-forth — comes into the tab as a transcript, so the agent starts with the full context, not a one-line summary.
- **A fix in the actual repo.** The agent works in the tab's working directory, so it reproduces and fixes against real code, not a description of it.
- **The answer, back where it was asked.** The resolution is posted to the same thread, addressed to the people who need it, so support and the customer hear back in the place they reported it.

One thread binds to one tab. A bound tab shows a green `@` indicator in the tab bar; hover it for the binding details, or right-click for the controls below.

## Setting it up

Everything lives in **Preferences → Integrations**. You need a **bot account** on your Mattermost server, and the bot has to be a member of any channel it should read or post in.

1. **Provider** — Mattermost. (The setting is a seam for other chat platforms later; Mattermost is what ships today.)
2. **Server URL** — the base URL of your Mattermost server, e.g. `https://chat.example.com`.
3. **Bot Token** — the access token of a Mattermost bot account (create one under **System Console → Integrations → Bot Accounts**). The token is stored locally and is **never exposed to agents** — no chat message and no MCP call can read it back.
4. Click **Test Connection**. On success maiTerm confirms the bot account it authenticated as (`Connected — bot account @yourbot`), so you know the token and URL are right before you rely on them.

Two more blocks on the same screen shape how the agent behaves — both optional, both covered below: **Message Authority** (who the agent trusts) and **Response Instructions** (how the agent writes).

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

## You stay in control

The agent is working against a live customer channel, so the design keeps you — not the chat participants — in charge of what it can do.

### Only @mentions reach the agent

The thread keeps flowing normally, but **only messages that `@mention` the bot are delivered into the session**. Everything else stays ambient — the agent can read it for context, but it doesn't act on it. That means the agent responds to deliberate asks, not to every message in a busy channel.

### Two authority tiers

Each delivered message is tagged by who sent it:

- **Authorized operators** — usernames you list under **Message Authority → Authorized usernames** (one per line). Their `@mentions` carry your full authority; the agent treats them as if you'd typed them yourself.
- **Everyone else** — support staff and other channel members are treated as **information and requests only**. The agent may investigate (read-only) and reply, but it will **not** take destructive, irreversible, or scope-expanding actions — deleting data, resetting state, or work beyond the reported issue — on their say-so. If a support message asks for something like that ("can we just wipe all that?"), the agent relays it to you and waits for sign-off rather than doing it.

Matching is by Mattermost username, so this is only as trustworthy as your server's identities. The authorized list is editable **only** in Preferences — no chat message can rewrite who the agent trusts.

### An operator kill switch

You can end a binding yourself at any time: right-click the tab and choose **End thread binding**. This is the human override — **severing a binding never depends on the agent cooperating**, and it posts nothing to the thread. Forwarding stops within a few seconds.

### A fix stays open until a human confirms

Posting a resolution no longer closes anything. The binding stays live, and the agent asks support to test and confirm:

- If someone replies that it's **still broken**, the agent keeps working — their messages keep arriving in the tab.
- If someone confirms it's **resolved**, the agent posts a brief sign-off and closes the thread out.

So a thread only closes on a human's confirmation, not on the agent's own belief that it's done.

### You're told when a reply can't be delivered

If someone `@mentions` the bot on a bound thread while its agent session isn't running, maiTerm doesn't silently swallow the message. It raises a notification — a toast or OS notification per your [notification mode](/features/agents/), deep-linking to the tab — so you know there's something waiting. The message isn't lost: the backlog is delivered as soon as you resume the session.

## Shaping how the agent writes

The **Response Instructions** field (Preferences → Integrations) is free-text guidance for how the agent communicates on threads — tone, formatting, what to include or leave out, when to post. It's handed to the agent whenever it picks up a thread, layered on top of the built-in defaults. Use it for house style, for example:

> Address the customer by name if the report includes it. Keep the support-facing summary under four sentences and free of jargon. Sign off as "— maiTerm bot".

Response Instructions govern **communication only**. The safety rules — what the agent may act on, and whose messages carry authority — are fixed and can't be changed here.

:::tip
Chat Threads pairs naturally with the rest of maiTerm's agent tooling. A thread can be worked by an agent that's also part of a [Mesh Workspace](/features/mesh-workspace/) or connected via an [Agent Bridge](/features/agent-bridge/) — and answered from your pocket over [maiLink](/features/mailink/) when a reply lands while you're away from your desk.
:::
