---
title: maiLink Companion
description: A phone companion that connects directly to maiTerm on your own computer — LAN-only, encrypted, no cloud — so you can watch and steer your agents from anywhere in the house.
---

maiLink is a companion app for your phone that connects **directly to maiTerm running on your own computer**. When an agent needs you — a permission prompt, a question, or it just finished — maiLink rings your phone; you read enough context to decide, and answer from wherever you are. You can also open any reachable agent as a chat and drive it proactively, unprompted.

maiLink is not a terminal. It renders a distilled chat transcript of an agent session and injects your replies back into it — the session itself never leaves your machine.

<figure class="phone-figure float-right">
  <img src="/screenshots/mailink-inbox.webp" alt="maiLink inbox on iPhone — agents grouped by whether they need your attention" />
  <figcaption>The inbox — the agents that need you, first.</figcaption>
</figure>

## No cloud in the data path

maiLink's defining design decision is what it *doesn't* do: it doesn't route your agents through anyone's server.

- **LAN only.** The phone talks to maiTerm over your local network. Your transcripts, prompts, and replies never transit the internet.
- **Encrypted and authenticated.** The connection is TLS end to end. maiTerm generates its own certificate, and the phone pins it by fingerprint — handed over out-of-band in the pairing QR — so a spoofed endpoint is rejected outright. Every request is authenticated with a per-device token minted at pairing time.
- **Off by default.** The LAN bridge doesn't listen until you enable it in Preferences, and no device can connect until you've explicitly paired it.
- **The existing agent integration is untouched.** maiTerm's MCP/IDE server stays bound to localhost as always — maiLink is a separate, explicitly-gated surface.

### The one exception: a content-free doorbell

iOS won't let an app listen for LAN connections while it's backgrounded, so one tiny piece of cloud is involved: a push **doorbell**, hosted on Cloudflare. When an agent needs you and no phone is actively connected, maiTerm sends a content-free wake through it — only the tab name and the kind of event travel, never terminal content, transcripts, or messages. The phone wakes, connects back over your LAN, and pulls the real content directly from your machine. The relay is multi-tenant with per-device capability auth — there is no shared secret, and nothing readable passes through it. You can also point maiTerm at a self-hosted relay in Preferences if you'd rather run your own bell.

### Away from home? Use WireGuard

maiLink deliberately has no cloud rendezvous, so out of the box it works within your own network — anywhere in the house or office. To reach your agents from outside, set up a **WireGuard VPN** back to your LAN rather than exposing anything to the internet. maiLink works over the tunnel exactly as it does at home, and your data stays on a link you control end to end.

## Pairing

Pairing is a QR scan, and you stay in control of every device:

1. Enable the maiLink bridge in **Preferences → AI Agents → maiLink Mobile Companion**.
2. Click **Pair a phone** — maiTerm displays a one-time QR code carrying the host, port, certificate fingerprint, and a single-use pairing code that expires in two minutes.
3. Scan it with the maiLink app. The phone verifies the pinned certificate and redeems the code for its own device token.

Every paired phone appears in the **Paired devices** list with its name and platform, and each is **individually revocable** — revoke one and it can no longer connect or ring, without disturbing the others. Each device holds its own token; there is no shared secret to rotate.

## What you can do from the phone

<figure class="phone-figure float-right">
  <img src="/screenshots/mailink-answer.webp" alt="Answering an agent's AskUserQuestion from the phone — choose an option or type a free-text reply" />
  <figcaption>Answer an agent's question — or interrupt it — without walking to your desk.</figcaption>
</figure>

- **Watch live transcripts.** Each agent's conversation streams to the phone per turn — a distilled chat view, not a terminal scrape. The thread shows the agent's model and a live context-window gauge, and a compaction shows up as a divider in the thread so you know when the agent's context was condensed.
- **Answer questions.** When an agent asks a structured question (`AskUserQuestion`), it arrives as an interactive card — single-select, multi-select, and "Other" free-text answers all work from the phone.
- **Approve permissions.** Permission prompts arrive the same way; approve or deny without walking to your desk.
- **Reply and interrupt.** Send a free-form message to a running agent, or interrupt it mid-turn — just like pressing `Esc` at the terminal.
- **Send images.** Snap a photo or pick a screenshot and send it to a local Claude Code session; it lands as an attachment the agent can view.
- **Resume a stopped agent.** A tab whose agent has ended (network drop, quit) stays reachable, so you can auto-resume it from your phone.

## Choosing what's reachable

<figure class="phone-figure float-left">
  <img src="/screenshots/mailink-transcript.webp" alt="A live agent transcript on the phone, with a bar to resume a dormant agent" />
  <figcaption>Every reachable agent — a live transcript you can pick up and resume.</figcaption>
</figure>

maiLink only ever surfaces **agent tabs** (Claude Code, Codex) — never plain shells. Which of those are reachable is up to you, under **Preferences → AI Agents → maiLink Mobile Companion**:

- **Default-on** — every agent tab is available, except ones you exclude with **Make unavailable in maiLink** in the tab's right-click menu.
- **Opt-in** — flip off *Make all tabs available in maiLink*, and only tabs (or whole workspaces) you explicitly mark **Make available in maiLink** appear on your phone.

:::note
maiLink builds on the same [agent integration](/features/agents/) pipeline as the rest of maiTerm — the hooks that drive tab indicators on the desktop are the same ones that ring your phone. Agents in a [Mesh Workspace](/features/mesh-workspace/) reach you through their native `AskUserQuestion` prompt, which is exactly what maiLink delivers to your pocket.
:::
