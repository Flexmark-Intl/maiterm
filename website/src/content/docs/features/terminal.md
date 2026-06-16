---
title: Terminal
description: Full-featured terminal emulator with xterm.js, scrollback persistence, and shell integration.
---

maiTerm's terminal does its heavy lifting in Rust: alacritty_terminal handles VTE parsing, the screen buffer, and the scrollback, while xterm.js is a thin renderer for just the visible viewport. Scrollback is persisted to SQLite for crash-safe storage — the state file stays tiny (~32KB) regardless of how much scrollback you have.

## Core Features

- **alacritty_terminal + xterm.js** — Rust-native VTE parsing, buffering, and scrollback in the backend; xterm.js renders just the visible viewport
- **Split panes** — horizontal and vertical splits, drag to resize, fully recursive binary tree layout
- **Multiple tabs** — per-pane tabs with activity indicators and completion detection; tabs scrolled out of view collapse into a searchable overflow menu that also shows how long each suspended tab has been parked
- **Composer dock** — a multi-line input docked below the terminal for writing long prompts comfortably (`Cmd+Shift+C`) — see [Composer Dock](#composer-dock)
- **Scrollback persistence** — saves and restores terminal state across restarts, on by default
- **SSH session cloning** — split an SSH session to get a second shell at the same remote CWD
- **SSH drop recovery** — when a connection drops unexpectedly, the tab keeps its title and offers a one-click reconnect
- **Multi-window** — open additional windows, duplicate windows with full tab context
- **Per-tab command history** — each tab maintains its own shell history, cloned tabs inherit it
- **File drop** — drag files onto a terminal to paste paths; over SSH, files are SCP'd to the remote CWD automatically, with live upload progress you can cancel
- **Image paste** — paste clipboard images (Cmd+V) into agent sessions (Claude Code, Codex) as temp file paths

## Composer Dock

A terminal gives you one line to type on — which is miserable for composing a long, careful prompt for a coding agent. The composer dock fixes that: a free-form, auto-growing text area docked below the terminal where Enter inserts newlines and `Cmd+Enter` sends. Press `Esc` to hop back to the terminal, `Cmd+Shift+C` to toggle the dock.

Sending is smarter than a paste. The composer checks what the foreground app actually supports: when it speaks bracketed paste (Claude Code, Codex, zsh, modern readline), your multi-line text arrives as a single submission with the line breaks intact; for older shells it falls back to sending line by line. Either way, what you wrote is what gets run.

It handles attachments too. Paste a screenshot or drop files onto the dock and they become **chips** above the input instead of raw paths cluttering your text. On send, the file paths are appended for you — and if the tab is an SSH session, the files are uploaded to the remote host first and referenced by their remote paths, same as dropping files on the terminal itself.

Drafts are per tab and persistent: switch tabs, restart maiTerm, and your half-written prompt is still there. When closed, the dock collapses to a small corner handle; whether new tabs start with it open is a preference under **Tabs**.

## Rendering

Because the screen buffer and scrollback live in the Rust backend, the frontend never holds more than a single screen of content — xterm.js runs with zero scrollback and simply paints the viewport the backend hands it. With nothing to scroll through on the frontend, GPU acceleration buys nothing, so maiTerm defaults to xterm.js's lightweight DOM renderer. That also sidesteps the glyph-ghosting artifacts the GPU renderers showed under maiTerm's full-frame streaming. A Canvas renderer is still available under **Terminal → Rendering** if you want to compare.

## Shell Integration

maiTerm supports the FinalTerm protocol (OSC 133) for command start/finish detection:

- **Tab indicators** — completed (checkmark/cross), at-prompt (›), and activity dot
- **OSC 7** — directory tracking, including remote CWD awareness through SSH
- **OSC 8 file hyperlinks** — the `l` command wraps `ls` to emit clickable file links
- **Remote install** — one-liner session setup or permanent `~/.bashrc`/`~/.zshrc` installation

## Tab Names

Tabs auto-update from terminal titles (OSC 0/2), but you can override with your own name — or combine both. Rename a tab "billing API debug" and it stays that way even as the terminal title changes underneath.

## Deep Clone Everything

Duplicate a tab and get *everything*: scrollback history, CWD, SSH session, the agent's resume command, tab name, notes, trigger variables. Or shallow clone for just the name and CWD. New tabs automatically inherit the workspace's most common working directory.

## Archive and Restore

Done with a session but not ready to lose it? Archive the tab. It disappears from your tab bar but preserves everything — scrollback, notes, trigger state. Restore it later and resume right where you left off.

## Suspend a Tab

Park a single session without closing it. Suspending a tab kills its PTY to free memory and CPU, but keeps the tab — and its scrollback — visible in the tab bar. A suspended tab still shows its last session's output behind a frosted-glass resume overlay, so you can see exactly what it was doing before you wake it. Click to resume and the shell spins back up. Handy for an idle SSH session or a finished build you want to keep around without it holding resources.

## Workspace Suspend & Resume

Suspend inactive workspaces to free resources — PTYs are killed and memory is released, but scrollback, CWD, SSH info, and all state are preserved. Click a suspended workspace to resume it instantly. Suspend individually, suspend all others, or configure auto-suspend after a timeout (15/30/60 min of inactivity).

## State Backup & Import

Export your entire maiTerm state — workspaces, tabs, scrollback, notes, preferences, triggers — to a backup file. Import it on a new machine or restore after a reset.

- **Manual export/import** from Preferences or the File menu
- **Scheduled backups** — hourly, daily, weekly, or monthly with a directory of your choice
- **Gzip compression** for scheduled backups
- **Auto-trim** old backups by configurable age
- **Selective import** — preview what's in a backup, pick which workspaces to import, choose overwrite or merge mode
- **Exclude scrollback** option to keep exports lightweight

## Reconnect a Dropped SSH Session

When an SSH session drops because of a network blip — not a clean `exit` — maiTerm notices the difference. Instead of resetting the tab to a bare local shell, it preserves the remote title and shows a **disconnected** badge in the tab bar. Click it to reconnect: maiTerm replays the same connection and drops you back into the directory you were in, so a flaky network doesn't cost you your place. A clean logout you did on purpose is left alone.

## Auto-Resume

Pin auto-resume settings so they survive across restarts. Configure SSH reconnection, remote CWD, and the resume command — maiTerm handles the rest. Edit settings anytime via context menu or replay with `Cmd+Opt+R`.
