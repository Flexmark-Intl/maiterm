---
title: Mesh Workspace
description: Turn a whole workspace into a mesh so every named agent in it can message any other by role — addressed, routed, and loop-controlled, with you in the cockpit.
---

[Agent Bridge](/features/agent-bridge/) connects exactly two agents. A **Mesh Workspace** generalizes that to N:M: flip a whole workspace into a mesh and every named agent tab in it can message any other agent by role name. Your frontend agent can ask the backend agent a question, the backend can loop in the infra agent, and the infra agent can reply to both — all without you copy-pasting between panes, and all where you can watch it happen.

## Why a mesh beats one giant agent

<figure class="mesh-figure float-right">
  <img src="/screenshots/mesh-cockpit.webp" alt="The maiTerm mesh cockpit — a node graph of six specialist agents, a topic list showing the conversations they're running with turn counts, and a status board" />
  <figcaption>The mesh cockpit: agents on the graph, their topics and turn counts, and a status board.</figcaption>
</figure>

The obvious way to work across several repositories is one agent with everything loaded into a single context. It's also the expensive way: that agent drags every codebase into one enormous context window and re-reads it turn after turn, burning tokens on code that's irrelevant to the question at hand — and the bigger the context gets, the worse the agent reasons about any one part of it.

A mesh inverts that. Each agent is small and **purpose-trained on its own repository** — its context holds one codebase, its purpose note tells it its role, and it goes deep instead of wide. When work crosses a boundary, agents exchange short, addressed messages — a precise question, a precise answer — instead of duplicating each other's worlds. The result is genuine **cross-repository collaboration** at a fraction of the token spend: you pay for a handful of focused exchanges, not for N copies of everything in one bloated context.

## Addressed, never broadcast

There is no broadcast. Every message is **addressed** to a specific recipient and routed off a stable handle, so renaming an agent never misroutes a message. An unknown or ambiguous recipient is a hard error that lists the current roster, never a silent drop — an agent always knows whether its message landed.

## Topics

Conversation in a mesh is organized into **topics**. A topic is owned by whoever starts it, and topics dedupe by normalized label — start "auth refactor" twice and you get the same topic, not two parallel threads talking past each other.

- **Ownership** — the agent (or you, the human) that opens a topic owns it.
- **Completion** — only the owner or the human can complete a topic. Once completed, the topic rejects further messages, so a wrapped-up thread can't be reopened by a stray turn.

## Loop control

By default a mesh flows freely — no caps, no pauses. When you want backstops against two agents thrashing indefinitely, three layered limits are available, each **off by default** and opt-in:

- **Soft turn cap** *(per topic)* — a back-and-forth pauses at a checkpoint when it hits the soft cap. A human resume lifts the pause by another round, so a productive exchange can keep going on your say-so.
- **Hard ceiling** — an absolute backstop a resume can't clear. When a topic hits the hard cap it stops for good.
- **Time-to-live** — a stale topic is force-paused once its TTL elapses, even if no one hit a turn cap.

Enable and tune any of the three in the **AI Agents** section of Preferences.

## The cockpit drawer

Open the cockpit with `Cmd+Shift+M`, or by clicking the **MESH** badge on a mesh workspace row in the sidebar. It gives you a single view of everything the agents are doing:

- **Live conversation graph** — the mesh's agents are arranged on a circle, with topic "stars" weighted by turn count. A pulse animation lights up topics that delivered a message recently, and a halo marks agents that are currently active. Click any node to jump straight to that agent's tab.
- **Topic list** — every topic with human **complete** and **resume** controls, so you can wrap a thread or lift a paused one without leaving the drawer.
- **Status board** — every agent with its live state and a **needs you** flag. The flag is the agent's *native* awaiting-input signal — an `AskUserQuestion` or a permission prompt — not guesswork parsed from terminal output, so it's deterministic: when an agent needs a decision it asks you directly, you get one deep-linked toast, and nothing else masquerades as needing attention. (Those same prompts are what ring your phone via [maiLink](/features/mailink/).)

## Stage + filmstrip view

When you want to focus on a hand-off between two agents, swap the workspace's split layout for the **stage**: a two-panel focused view with a live, CSS-scaled **filmstrip** of the other agents along the side. Click a filmstrip tile to bring that agent to the left panel; `Shift`+click to put it on the right. An **Exit** / **Tab view** button returns you to the normal split layout.

Promoting and demoting agents this way never reflows or respawns a terminal — the stage rearranges what you see, not the live sessions underneath.

## Enabling a mesh + readiness

Enabling a mesh opens a **pre-flight readiness modal** that inventories every tab in the workspace and tells you exactly where each one stands:

- **Ready** — registered and good to go.
- **Not yet registered** — offers to **Send `/maiterm init`** so the agent registers itself.
- **Suspended** — offers **Wake** for that tab, or **Wake all** to bring back everything at once.
- **Unnamed** — offers an inline rename, since a mesh agent needs a role name to be addressable.

Registering an agent isn't a blind paste. A tab coming out of a resume is often sitting on a startup dialog — Claude Code's "restore as is / compact first" prompt, say — where a typed command would answer *the dialog* instead of registering. maiTerm answers any such dialog first, waits for the terminal to actually go quiet (so a long compaction finishes before anything is sent), and then delivers the registration command exactly once. The response window starts when the command is genuinely delivered, so a slow-starting agent no longer reads as a failure; a row that really does time out keeps a **Retry** button rather than dead-ending.

After a restart, the mesh **auto-rechecks** readiness and offers to wake or re-init any agents that dropped, so your mesh comes back the way you left it. Each agent can also carry an optional **purpose** note — a one-line scope steer that tells it what its role in the mesh is — and those purposes persist across restarts.

You can also ready a whole mesh **from your phone**: mesh workspaces are badged in the [maiLink](/features/mailink/) inbox with a one-tap **Initialize all** that triages each member the same way — register the running-but-unregistered, resume the exited, leave the live ones alone.

:::note
Mesh Workspace builds on the same [agent integration](/features/agents/) pipeline as [Agent Bridge](/features/agent-bridge/). It needs supported agents (Claude Code or Codex) running in the tabs you want to mesh, and works over SSH through the same reverse-tunnel MCP bridge.
:::
