---
title: Deshittification
description: Switch off the parts of a coding agent that serve its vendor rather than you — telemetry, feedback nags, and self-promotion in your commit history.
---

Coding agents ship with behaviour that works for the vendor rather than for you: usage telemetry, uploaded error reports, in-session feedback surveys, and a `Co-Authored-By: Claude` trailer stamped into your commit history. **Preferences → Deshittification** switches those off — one rule at a time, or the whole group with a single toggle.

There's one group today, **Claude Code**. maiTerm edits the agent's *own* configuration — `~/.claude/settings.json` and your global git config — so the rules hold in every terminal you run the agent in, not just maiTerm's. Claude Code picks them up on its next session.

## The rules

| Rule | What it does |
|------|--------------|
| **Disable telemetry** | Stops usage metrics being sent to Anthropic |
| **Disable error reporting** | Stops crash and error reports being uploaded |
| **Remove the `/bug` command** | The command stops being offered |
| **Remove the `/feedback` command** | The command stops being offered |
| **Suppress feedback surveys** | Stops the in-session survey prompts |
| **No `Co-Authored-By` in commits** | Claude Code stops writing the trailer in the first place |
| **Strip agent credit at commit time** | A `commit-msg` hook deletes `Co-Authored-By: Claude` and `Generated with Claude Code` lines from your commit messages |

The last two are deliberately a pair. The setting is Anthropic's to honour; the hook is yours, and it strips the trailers whether or not the setting is still respected.

## The toggle *is* the state on disk

None of this is stored as a maiTerm preference. Each toggle reads the agent's real configuration every time you open the section, so what's on disk is what you see: undo a rule by hand somewhere else and it reads back as off here, with nothing to re-assert it behind your back.

## Your repo's own hooks still run

The commit-msg rule needs git's global `core.hooksPath`, which would otherwise silently disable every repository's own `.git/hooks/*`. maiTerm's managed directory therefore ships a passthrough for each standard hook that runs your repository's hook of the same name — pre-commit, pre-push, the server-side gates, and the rest — so nothing you already rely on stops firing.

Two limits worth knowing:

- **A repository that sets its own `core.hooksPath` wins.** husky, lefthook and simple-git-hooks all do, so the trailer-stripping hook doesn't reach those repos. The setting above still applies there.
- **maiTerm never overwrites someone else's `core.hooksPath`.** If your global config already points at a directory maiTerm didn't write, the rule reports itself blocked and names the path rather than taking it over.

## It travels to the hosts you bridge to

An agent on a remote server gets the same treatment. The rules are applied on each [SSH bridge](/features/agents/#ssh-mcp-bridge) connect, so a host you work on through maiTerm matches your Mac. Your Mac is the source of truth in both directions: switch a rule off here and it's removed there on the next connect, so an undo actually travels.

Deshittification never gates a bridge — a rule that can't be applied on a remote host doesn't stop the connection or fail the setup. A host you only ever `ssh` into by hand is never touched, and neither is one belonging to a user who never opens the section.

## When a rule can't be applied

A rule maiTerm can't safely write is greyed out and says why — an unreadable `~/.claude/settings.json`, or a `core.hooksPath` belonging to someone else. One cause often blocks several rules at once, so a shared reason is stated once above the list rather than repeated on every row; a reason that belongs to a single rule sits on that rule.

Blocked rules are left out of the group's arithmetic: the header counts them separately (*"4 of 5 applied, 2 unavailable"*), and the group toggle only goes dead when there's nothing left it could move.
