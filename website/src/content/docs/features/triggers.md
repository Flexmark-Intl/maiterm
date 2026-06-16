---
title: Triggers & Automation
description: Regex triggers that watch terminal output and fire actions — notifications, commands, and variable capture.
---

Triggers watch your terminal output for patterns and fire actions automatically. Agent tracking (Claude Code and Codex) has moved to [hooks](/features/claude-code/#agent-hooks), but the trigger engine is fully available for your own custom automation.

## How Triggers Work

1. Terminal output is stripped of ANSI escape codes
2. Output is buffered per-tab (with TUI redraw detection)
3. Regex patterns are tested against the buffer
4. Matching text fires configured actions
5. Matched text is consumed from the buffer

## Match Modes

- **Regex** — full regular expression matching (default)
- **Plain text** — simple substring matching
- **Variable condition** — evaluates expressions using captured variables

### Variable Conditions

Expression parser supporting complex conditions:

```
sessionId && !resumed
status == "waiting"
a || b && c
x != "done"
```

Operators: `&&`, `||`, `!`, `==`, `!=`

## Actions

| Action | Description |
|--------|-------------|
| `notify` | Send a notification (toast or OS notification) |
| `send_command` | Write a command to the PTY |
| `enable_auto_resume` | Enable auto-resume for the tab |
| `replay_auto_resume` | Re-send the stored auto-resume command to the PTY |
| `set_tab_state` | Set the tab's state indicator |

## Variables

Capture groups in regex patterns can be mapped to named variables:

- Variables are persisted per-tab in `trigger_variables`
- Referenced with `%varName` syntax
- Used in tab titles, auto-resume commands, notification messages
- Cloned when duplicating tabs

## Use Cases

With agent tracking now handled by [hooks](/features/claude-code/#agent-hooks), triggers are best used for your own custom automation:

- Watch for build failures and send a notification
- Detect SSH disconnects and auto-reconnect
- Capture environment variables from command output
- Fire a command when a deploy prompt appears
- Set tab state indicators based on output patterns

## Tab-Level Scoping

Triggers can be scoped to specific tabs for per-tab pattern matching. This lets you set up different automation for different contexts — one trigger watching for build failures in your dev tab, another watching for deploy prompts in your staging tab — without them interfering with each other.

## Notifications

Triggers can fire notifications through maiTerm's three-mode notification system:

- **Auto** (default) — in-app toasts when window is focused, OS notifications when not
- **In-app** — always show in-app toasts
- **Native** — always use OS notifications
- **Disabled** — no notifications

Notifications support deep-linking — clicking a toast or OS notification navigates to the source workspace and tab. Sound alerts are configurable with volume control.
