---
title: Agent Bridge
description: Connect two running agent sessions so they can collaborate directly — while you stay the one making the calls.
---

Agent Bridge connects two running agent sessions so they can talk to each other directly — say one in your local project and another SSH'd into a related service. The two sides can be any supported agents, so a **Claude Code** and a **Codex** can bridge across runtimes just as easily as two of the same. Instead of you copy-pasting context back and forth between two agents, you bridge them once and they collaborate: one asks the other questions, requests research, or shares context, and the replies arrive as new turns in each session. You watch the whole exchange, and you stay the one making the decisions.

## When to use it

Agent Bridge shines whenever two projects need to talk:

- **A client and its API** — your frontend agent asks the backend agent about an endpoint's exact shape instead of guessing.
- **Local and remote** — bridge a local agent session to one running over SSH on the server it deploys to, so each can answer the other's questions about the live environment.
- **A library and its consumer** — the app-side agent asks the library-side agent how a function is meant to be called, and the library-side agent learns how it's actually used in the wild.

Two related codebases, two agents that each know their own half deeply — Agent Bridge lets them fill in each other's blind spots without you playing messenger.

## Bridging two agents

From a terminal tab running an agent, press `Cmd+Shift+L` — or right-click and choose **Create Agent Bridge…** — to open the **Agent Bridge** picker. Pick another running agent session, and the two are bridged. Candidates are sorted by most recent activity, and a search box filters by tab name, workspace, or working directory — so finding the right peer stays quick even with a fleet of agents running.

The picker offers two modes:

- **Fork into new pane** *(default for agents that support it)* — maiTerm forks the chosen session into a split pane right beside your current one. The fork inherits the target session's full context but runs as an isolated peer, so reaching out to it never disturbs the original. You end up with both sides of the bridge side by side, ready to interact with each. Forking is a Claude Code capability; when the target is a Codex session, the picker uses **Connect existing tab** instead.
- **Connect existing tab** — link two already-running agent tabs directly, with no fork and no new pane. This works for any pairing, including across runtimes (Claude Code ↔ Codex), and is the path Codex always uses. It's idempotent: re-selecting your current partner repairs a broken link in place, and it won't hijack a tab that's already bridged to someone else.

There's also an optional **purpose** field. Describe the peer for your own agent — what it's an expert on, how it should be used — and that context is handed to your agent so it knows what the bridge is for instead of firing off questions blindly.

## How the agents talk

Once bridged, each agent reaches the other through the `sendToBridgedAgent` MCP tool. Messaging is **asynchronous**: a message is injected as a real prompt turn in the recipient's pane, the recipient works on it, and its reply comes back later as a new turn in the sender's pane.

Because every message lands as a visible turn in a real terminal, **you see the entire conversation as it happens** — and you can step in at any point. Hit `Esc` to interrupt either side, just like any agent session.

## Everyone knows their role

The agents are kept fully aware of the situation they're in:

- **They know they're talking to a peer, not to you.** maiTerm stamps each message with the sender's identity — tab, workspace, and working directory — pulled from its own session registry. An agent can't fake being the human operator, so the recipient always knows a message came from a peer agent.
- **They know you're still in charge.** Rather than letting the calling agent interrogate its new peer unprompted, the opener tells it to check in with you first — summarize what the peer offers, propose a few things it could ask — and wait for your direction before reaching out. You remain the decision-maker the agents defer to.

## Bridges survive restarts

A bridge is durable. The pairing is saved on both tabs, so it survives quitting and reopening maiTerm. On the next launch the bridge is rebuilt automatically, and because an agent can pick up a fresh session id when it auto-resumes, maiTerm re-binds the pair instead of dropping the link. If one side ends its session, the bridge is suspended rather than torn down — when that agent resumes, it reconnects. Only an explicit disconnect or closing the tab removes a bridge for good.

Bridged tabs show a small link badge in the tab bar so you can see which sessions are paired at a glance. To break a bridge, right-click the tab and choose **Disconnect Agent Bridge**.

:::note
Agent Bridge is part of maiTerm's [agent integration](/features/claude-code/). It needs a supported agent (Claude Code or Codex) running in both tabs, and works over SSH through the same reverse-tunnel MCP bridge that powers the rest of the integration.
:::
