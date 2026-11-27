---
title: Agent Defaults
description: Set the model, effort, advisor and transcript retention each coding agent starts every session with, written into the agent's own settings file and kept the same on every SSH host you bridge to.
---

Every coding agent keeps its own defaults: which model a new session starts on, how hard it thinks, which advisor Claude consults, how long transcripts are kept. Changing them usually means `/model` or `/advisor` in every session, or hand-editing a settings file on every machine you work on. **Preferences → Agent Defaults** sets them in one place.

maiTerm writes each value into the agent's **own** settings file, so it holds in every terminal you run the agent in, not just maiTerm's. A session's own `/model`, `/effort` or launch flag still wins for that session. Changes apply to new sessions; an agent already running picks one up when it restarts.

It's the sister of [Deshittification](/features/deshittification/): that section switches things off, this one picks values.

## The settings

There's a part per agent, headed with the file it edits. A part whose agent isn't installed is shown collapsed as *not installed*; its values can still be set and wait in the file.

| Agent | File | Settings |
|-------|------|----------|
| **Claude Code** | `~/.claude/settings.json` | **Advisor**, **Default model**, **Effort** (one row per model, since Claude keeps effort per model), **Keep transcripts (days)** |
| **Codex** | `~/.codex/config.toml` | **Default model**, **Reasoning effort**, **Approvals reviewer**, **Screen** |
| **Antigravity** | `~/.gemini/antigravity-cli/settings.json` | **Agent mode**, **Thinking**, **Messages sent while it works** |

Every control has **Agent's default**, which removes the setting from the file rather than writing today's default, so when the agent's own default changes, you get the new one. maiTerm touches only the keys this section offers; everything else in the file is kept, and a Codex `config.toml` keeps its layout and comments. Antigravity's model and effort aren't kept in its settings file, so they aren't offered.

Codex's **Screen** is **Scrollback** or **Fullscreen** (`[tui] fullscreen_transcript`). Fullscreen, Codex's default, takes over the whole terminal on the alternate screen, so nothing of the conversation reaches the terminal's own history: you can't scroll back through it, search it or keep it across a restart. **Scrollback** runs Codex inline, so its conversation stays in the tab's history like any other output.

With your own `CLAUDE_CONFIG_DIR` and no [managed accounts](/features/accounts/), the Claude part edits the `settings.json` your tabs actually read. With managed accounts, one file reaches every account.

## Only what you tick

maiTerm changes **only the settings you tick**. Each row has a tick; unticked, its control is read-only and maiTerm never changes that setting, here or on a host. Ticking records what the file holds now and writes nothing. **maiTerm sets the ticked settings**, at the top, turns the whole section off without forgetting your ticks.

The section reads each file every time you open it, so a value you change with the agent's own `/config` shows here. If it differs from what maiTerm has recorded for your hosts, the row says so and offers to **give hosts this machine's** value.

## It follows you to your SSH hosts

A ticked setting is set on every SSH host a tab [bridges](/features/agents/#ssh-mcp-bridge) to, straight away for hosts bridged now and again each time one connects, so every machine follows the same value. **Agent's default** removes it there too. An unticked setting is left alone on every host, which is how a host keeps a value of its own.

Hosts are given the value you chose, not whatever this machine's file happens to hold, so a hand edit here never turns into a removal on every host. On a host, nothing is created for an agent that isn't installed there, and a file that doesn't parse is left alone. The host needs `python3`. maiTerm edits `~/.claude/settings.json` there, whatever `CLAUDE_CONFIG_DIR` the host uses.

## The advisor

The advisor is a stronger model Claude consults before and after substantial work: **Fable**, **Opus** or **Sonnet**. It must be at least as capable as the model it advises, or Claude skips it. Some advisor models bill to usage credits.

Claude only turns the advisor on behind a feature flag it fetches, and with telemetry off — [Deshittification](/features/deshittification/)'s **Disable telemetry**, for one — it never fetches it, so the advisor is silently ignored. maiTerm writes the switch that bypasses the flag (`CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL`) together with the advisor, and removes it with it. If an advisor is set but your settings would still keep it off, the row says so.

[Hive](/features/hive/) workers consult an advisor too. By default it's the one set here; **Workers and access…** in the Hive's settings can pick another or none. Each consultation costs, so a Hive multiplies it by its workers.

## Transcripts: 500 days, not 30

Claude Code deletes transcripts older than **30 days** when a session starts, unless told otherwise, and resume, [maiLink](/features/mailink/) and the [Loom](/features/loom/) all read them. So maiTerm sets **Keep transcripts** to **500 days**, once, on this machine, and only where nothing sets a retention already. A value you'd already set is left alone, and once it's applied a change in either direction sticks. It ticks nothing, so it never reaches a host on its own.

Tick **Keep transcripts** and leave it empty, and every host falls back to Claude's 30 days and deletes its older chats.

## When a file can't be read

A settings file maiTerm can't parse blocks its agent's part, with the error shown, and is never overwritten. A link to a missing file is refused rather than replaced, and a dotfiles symlink stays a symlink: maiTerm writes through it to its target.
