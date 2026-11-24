---
title: Deshittification
description: Switch off the parts of a coding agent that serve its vendor rather than you — telemetry, feedback nags, Remote Control, other people's artifact comments arriving as prompts, self-promotion in your commit history, the memory-write prompts Claude Code asks by mistake, and a fullscreen renderer that keeps sessions out of your scrollback.
---

Coding agents ship with behaviour that works for the vendor rather than for you: usage telemetry, uploaded error reports, in-session feedback surveys, Remote Control exposing your sessions to claude.ai and the mobile app, comments other people leave on your claude.ai artifacts arriving in your session as prompts, and a `Co-Authored-By: Claude` trailer stamped into your commit history — plus a permission prompt for every memory Claude Code writes, which it never meant to ask, and a fullscreen renderer new installs switch on by themselves that keeps a session out of your scrollback. **Preferences → Deshittification** switches those off — one rule at a time, or the whole group with a single toggle.

There's one group today, **Claude Code**. maiTerm edits the agent's *own* configuration — `~/.claude/settings.json` and your global git config — so the rules hold in every terminal you run the agent in, not just maiTerm's. Claude Code picks them up on its next session. To pick values rather than switch things off — each agent's model, effort, advisor and how long it keeps transcripts — see [Agent Defaults](/features/agent-defaults/).

## The rules

| Rule | What it does |
|------|--------------|
| **Disable telemetry** | Stops usage metrics being sent to Anthropic |
| **Disable error reporting** | Stops crash and error reports being uploaded |
| **Remove the `/bug` command** | The command stops being offered |
| **Remove the `/feedback` command** | The command stops being offered |
| **Suppress feedback surveys** | Stops the in-session survey prompts |
| **Disable Remote Control** | Sets `disableRemoteControl: true` and `remoteControlAtStartup: false`, so your sessions aren't exposed to claude.ai/code or the mobile app and the bridge never auto-starts. **On by default** |
| **Ignore artifact comments** | Sets `CLAUDE_CODE_ARTIFACT_COMMENTS=0` and `CLAUDE_CODE_ARTIFACT_COMMENTS_AUTOREACT=0`. Comments people leave on an artifact Claude published to claude.ai stop arriving in your session as prompts, and Claude stops starting work on them by itself, so other people's words aren't typed into your agent. **On by default** |
| **Keep the classic terminal UI** | Sets `tui: "default"`. New Claude Code installs can switch themselves to the fullscreen renderer, which draws on the alternate screen: nothing it prints reaches the terminal's scrollback, so in maiTerm you can't scroll back through a session, search it or keep it across a restart, and you can only select what's on screen. Run `/tui fullscreen` inside Claude Code to opt back in, and this rule reads as off. **On by default** |
| **Stop asking to write memory** | A permission hook approves Claude Code's writes to Markdown files in its own memory directory, which it otherwise asks about one by one, and refuses outright in auto mode, wherever its config directory runs through a symlink. Anything that doesn't really lead into a memory directory still prompts. A shell command that would ask you about the memory folder — a write, a delete or move, or a command that merely names the folder — is refused at once instead of waiting for you: the agent is told to use its Write or Edit tool, to ask you to run a delete or rename, or to leave the folder out of a command that doesn't touch memory. **On by default** |
| **No `Co-Authored-By` in commits** | Claude Code stops writing the trailer in the first place |
| **Strip agent credit at commit time** | A `commit-msg` hook deletes `Co-Authored-By: Claude` and `Generated with Claude Code` lines from your commit messages |

The last two are deliberately a pair. The setting is Anthropic's to honour; the hook is yours, and it strips the trailers whether or not the setting is still respected.

## The toggle *is* the state on disk

None of this is stored as a maiTerm preference. Each toggle reads the agent's real configuration every time you open the section, so what's on disk is what you see: undo a rule by hand somewhere else and it reads back as off here, with nothing to re-assert it behind your back.

## Default-on rules

**Disable Remote Control**, **Ignore artifact comments**, **Keep the classic terminal UI** and **Stop asking to write memory** are the rules maiTerm turns on without being asked. Each is applied once, the first time a maiTerm with that rule launches; after that it's like every other rule. Switch one off and it stays off — maiTerm remembers the rule is settled (in `~/.maiterm/`, so a backup restore or a second maiTerm build doesn't forget) and never re-asserts it. If you'd already chosen otherwise yourself, the default leaves your choice alone: Remote Control switched on, `CLAUDE_CODE_ARTIFACT_COMMENTS` or `CLAUDE_CODE_ARTIFACT_COMMENTS_AUTOREACT` set on in Claude Code's `settings.json`, or the fullscreen renderer picked — `tui: "fullscreen"` (what `/tui fullscreen` writes) or `CLAUDE_CODE_NO_FLICKER` set on there. maiTerm reads those values as Claude Code does — `1`, `true`, `yes` or `on` is on, `0`, `false`, `no` or `off` is off, in any case — and a value Claude Code reads as neither counts as no choice.

## Your repo's own hooks still run

The commit-msg rule needs git's global `core.hooksPath`, which would otherwise silently disable every repository's own `.git/hooks/*`. maiTerm's managed directory therefore ships a passthrough for each standard hook that runs your repository's hook of the same name — pre-commit, pre-push, the server-side gates, and the rest — so nothing you already rely on stops firing.

Two limits worth knowing:

- **A repository that sets its own `core.hooksPath` wins.** husky, lefthook and simple-git-hooks all do, so the trailer-stripping hook doesn't reach those repos. The setting above still applies there.
- **maiTerm never overwrites someone else's `core.hooksPath`.** If your global config already points at a directory maiTerm didn't write, the rule reports itself blocked and names the path rather than taking it over.

## It travels to the hosts you bridge to

An agent on a remote server gets the same treatment. The rules are applied on each [SSH bridge](/features/agents/#ssh-mcp-bridge) connect, so a host you work on through maiTerm matches your Mac. Your Mac is the source of truth in both directions: switch a rule off here and it's removed there on the next connect, so an undo actually travels.

Deshittification never gates a bridge — a rule that can't be applied on a remote host doesn't stop the connection or fail the setup. A host you only ever `ssh` into by hand is never touched. A bridged host gets the default-on rules like your Mac does, and nothing else unless you switch it on. **Keep the classic terminal UI** writes `tui: "default"` on a host only where its `settings.json` has no `tui` setting yet, so a `/tui fullscreen` chosen there survives every connect; switching the rule off here removes only maiTerm's `"default"`.

## When a rule can't be applied

A rule maiTerm can't safely write is greyed out and says why — an unreadable `~/.claude/settings.json`, or a `core.hooksPath` belonging to someone else. One cause often blocks several rules at once, so a shared reason is stated once above the list rather than repeated on every row; a reason that belongs to a single rule sits on that rule.

Blocked rules are left out of the group's arithmetic: the header counts them separately (*"4 of 5 applied, 2 unavailable"*), and the group toggle only goes dead when there's nothing left it could move.
