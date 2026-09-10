---
title: maiLink Companion
description: A phone companion that connects directly to maiTerm on your own computer — LAN-only, encrypted, no cloud — so you can watch and steer your agents from anywhere in the house.
---

maiLink is a companion app for your phone that connects **directly to maiTerm running on your own computer**. When an agent needs you — a permission prompt, a question, or it just finished — maiLink rings your phone; you read enough context to decide, and answer from wherever you are. You can also open any reachable agent as a chat and drive it proactively, unprompted. The ring fires only when an agent actually crosses into needing you, so merely opening the app or restoring a session never pushes a phantom "finished."

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

iOS won't let an app listen for LAN connections while it's backgrounded, so one tiny piece of cloud is involved: a push **doorbell**, hosted on Cloudflare. When an agent needs you and no phone is actively connected, maiTerm sends a content-free wake through it — only the tab name and the kind of event travel, never terminal content, transcripts, or messages. The phone wakes, connects back over your LAN, and pulls the real content directly from your machine. "Actively connected" is verified rather than assumed: an app iOS has suspended in your pocket leaves its connection looking perfectly alive, so maiTerm heartbeats it every twenty seconds and a phone that stops answering is returned to doorbell coverage within one interval. The relay is multi-tenant with per-device capability auth — there is no shared secret, and nothing readable passes through it. You can also point maiTerm at a self-hosted relay in Preferences if you'd rather run your own bell.

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

- **Watch live transcripts.** Each agent's conversation streams to the phone per turn — a distilled chat view, not a terminal scrape — whether the agent runs locally, over SSH, or is a Codex session. A Claude agent running on a remote host over SSH gets the same per-turn thread as a local one: maiTerm mirrors its conversation log from the remote machine to your computer as it works, keeping a terminal snapshot only as a fallback for when the connection is unavailable. The thread shows the agent's model, its **reasoning effort** (low / medium / high / xhigh / max) and a live context-window gauge, and a compaction shows up as a divider in the thread so you know when the agent's context was condensed. The effort badge is read from what Claude Code itself records with each turn, and it stops recording it once a session has been **resumed** — so a long-lived session shows no effort at all. maiTerm leaves it blank rather than falling back to the last value it saw, because that value predates the resume and you may have changed the setting since; an absent badge is visibly absent, while a stale one is indistinguishable from a live one. Agent-to-agent [mesh](/features/mesh-workspace/) and [bridge](/features/agent-bridge/) messages render as their own kind of turn — who it came from, which topic it belongs to, and the peer's actual words — rather than being dropped from the thread or buried as an unreadable tool call, and a fan-out of subagents shows each one's task instead of a run of identical chips.
- **Answer questions.** When an agent asks a structured question (`AskUserQuestion`), it arrives as an interactive card — single-select, multi-select, and "Other" free-text answers all work from the phone. The card's countdown reflects whether your Claude Code build actually expires an unanswered question — newer builds leave them open by default, so a live question never looks falsely expired. A given question is answered **once and only once**: answering it drives Claude Code's own selector, and if that attempt doesn't take, maiLink sends what you wrote as an ordinary message instead of trying again. A blind retry would start from an unknown row in a selector that can't be re-homed, and could record you as having said something you didn't — sending your words as a message can't.
- **Approve permissions.** Permission prompts arrive the same way, and the card names exactly what you're approving — `Bash(rm -rf ./dist) — approve?`, not just the tool name — so you decide with the full picture. Codex prompts work too: maiLink sends the keystroke that matches Codex's own variable-length approval list, so your choice can't land on the wrong option.
- **Read and edit the task board.** The [task list](/features/tasks/) a tab shares with its agent mirrors to the phone — any runtime, local or over SSH — grouped by workstream and showing each row in the lane the desktop actually renders it in, including one held **Blocked** by an unfinished prerequisite. You can add a task, retitle one, move it between lanes, or reassign it to another tab; the write lands in maiTerm even while the Mac's screen is asleep, and the desktop board picks it up without a refresh. **Start** a task and the tab's agent is actually told to pick it up, rather than the row quietly going Active with nobody working on it — and the phone reports which happened: the agent was told, [Overlord](/features/overlord/) will relay it, or the row is Active and nobody was told (a task with no tab assigned, for instance).
- **Run [Overlord](/features/overlord/) from the phone.** With Overlord on, its escalations and proposals mirror to the phone: approve or dismiss a proposal, clear an escalation, fire a rule at a tab, or drive one. A new escalation rings the doorbell. These actions run in the window that owns the engine, so one sent to a sleeping desktop reports as *sent, not confirmed* rather than claiming to be done — the next snapshot settles it.
- **See the goal it's being held to.** A `/goal` installs a session-scoped check: the agent can't end its turn until a judge decides the condition holds, and every attempt that falls short sends it back with a written account of what's still missing. The thread shows that condition, how many evaluations it has been through, and the judge's latest verdict in full — away from your desk, that's the closest thing to an answer for "is it going to finish, and what's left?". Each verdict also lands as its own row in the transcript, so the "not yet, because…" rulings read as a progress log where they happened. A goal that's met, cleared by hand, or ruled impossible by the judge is reported as such until the conversation moves past it, so a completion can't slip by unseen. Claude sessions only; because it's read from the conversation log rather than from a running process, SSH agents show their goals too.
- **See what it's working on right now.** A busy agent used to say only "working…". The thread now carries a live status line naming the tool it's running and what it's running it on — `Bash — npm run build`, not just "Bash" — which clears the moment the turn ends, so an idle agent never sits under the last thing it happened to do.
- **Follow the work it hands off.** When an agent delegates to a subagent — a code review, a research pass, the kind of thing that takes minutes — that delegation gets its own row: what it was asked to do, how long it has been at it, and the subagent's own latest line of progress. A finished one keeps its last words rather than collapsing to a bare "done". SSH agents get the roster too, though the progress line lives on the remote host and doesn't travel; the rest does.
- **See what's running in the background, and stop it.** Background shells the agent started show up as a roster — what each one is running, the agent's own label for it, and how it ended if it has. The ones still alive get a **Stop** button. Liveness is confirmed against your machine's process list rather than taken from the transcript, which records nothing when a background process exits — so Stop is only ever offered for a process that genuinely exists. This covers Claude agents running on your own computer; an SSH agent's background shells belong to the remote host, where maiTerm can neither confirm them nor signal them, so those sessions report none.
- **Reply and interrupt.** Send a free-form message to a running agent, or interrupt it mid-turn — just like pressing `Esc` at the terminal. The tab settles straight back to idle when you stop it, rather than sitting on "Working" until the agent's next turn. Stopping a turn also clears the input the agent restores into its composer, so the next thing you send can't merge into the message you just cancelled.
- **Send while it's busy — and take it back.** A message typed mid-turn is queued by Claude Code rather than delivered, and it appears in the thread marked as queued instead of leaving you wondering whether it landed. While it's still waiting you can **cancel** it. That's offered when exactly one message is queued: Claude Code's own recall pulls the whole queue back at once, so with two waiting there's no way to retract just the one you meant.
- **Switch a Claude session's model.** The choices come from maiTerm rather than from a list compiled into the phone, so a model your account has been offered shows up in the picker without waiting for a new phone build. Two sources feed it: the models Anthropic has pushed to your account, and a small built-in table of the standard tiers. Each entry carries the exact value maiTerm will send, so nothing is guessed on your behalf — and because a model change is confirmed by the *next* reply naming it, a switch that doesn't take is visible rather than assumed.
- **Start a new conversation.** Spawn a fresh session alongside the one you're reading — same host, same working directory, new agent, no shared history. Useful when a thread has drifted and you want a clean start without walking back to your desk. It comes up connected and initialized on its own, in the background, so your desktop view isn't yanked out from under you.
- **Receive files the agent sends you.** An agent can hand you files directly — a build artefact, a report it just wrote, a screenshot of a failing test. They arrive in that tab's chat and in a cross-chat **Files** list, where they can be previewed or saved. Any file type, not just images: images and video play in place and everything else downloads. SSH tabs are ordinary here — the agent passes its own remote paths and maiTerm fetches them back over the tunnel that's already up; only a tab whose tunnel is down fails, and it says so. A file's bytes are copied at the moment it's sent, so rewriting or truncating it afterwards can't retroactively change what you were already given.
- **Send images.** Snap a photo or pick a screenshot — or several at once — and send them to a Claude Code session; they land as attachments the agent can view. This works whether the agent runs on your computer or on a remote host over SSH: for an SSH agent, maiTerm streams the images to the remote machine before referencing them, all-or-nothing, so a failed transfer never leaves a half-filled prompt.
- **Wake a tab that isn't answering.** A tab whose agent has ended (network drop, quit) stays reachable, and so does one that's running but hasn't registered with maiTerm. Either way the fix is a single **Initialize** on the thread — available on any tab, not just [meshed](/features/mesh-workspace/) ones. maiTerm picks the remedy itself, based on whether the agent process is actually alive: a live agent is re-registered, a dead one is auto-resumed. Sending a message to a tab in that state wakes it the same way first, so a message can never be typed at a bare shell prompt and run as a command; if it can't be delivered, the phone is told why rather than left to guess.

## Managing tabs and workspaces from the phone

maiLink isn't only a window onto running agents — the housekeeping you'd otherwise walk back to your desk for works from the couch too. Every one of these actions lands on the desktop immediately: the tab strip on every open maiTerm window updates live, and other paired phones pick the change up on their next refresh.

- **Rename a tab.** Give a thread a name that means something. The new name is the tab's own name on the desktop — it persists, and it survives a resume.
- **Archive a tab.** Tuck a tab away recoverably. It leaves the tab bar but keeps everything — scrollback, notes, triggers, its resume command — exactly like [archiving from the desktop](/features/terminal/#archive-and-restore).
- **Browse and restore what you archived.** A dedicated **archived** list shows everything you've tucked away, across all workspaces, newest first. Restore one and it comes back into its workspace, respawns its terminal, and resumes its agent.
- **Close a tab.** For the ones you're done with. This is the destructive option — the terminal is killed and the scrollback deleted, with no archive entry to come back to.
- **Wake a suspended workspace.** A suspended workspace has no terminals running, so its tabs show a **Resume workspace** control instead of a dead Initialize. Tapping it resumes the workspace on your computer — respawning exactly the tabs that were live when you suspended it and re-initializing their agents — and the thread becomes usable as soon as it's back.
- **Ready a whole mesh in one tap.** [Mesh workspaces](/features/mesh-workspace/) are badged in the inbox, and a single **Initialize all** readies every member. Each agent is triaged on its own: one that's running but not yet registered is simply registered, one that has exited is resumed, and one that's already live is left alone.

## Fast on big sessions

maiLink stays responsive on real workloads — a hundred-tab window and sessions whose logs run to hundreds of megabytes. Opening a thread reads only the tail of the conversation it actually shows, tab metadata is answered from an index rather than by walking every buffer, and maiTerm's own background loops work from lightweight summaries instead of rebuilding the full chat list every couple of seconds. In practice: thread opens are immediate, and the app doesn't get slower the longer maiTerm has been running.

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
