---
title: Files
description: A project tree beside your terminals that follows the tab you're in, shows git status, and lights up the files your agents read and change — with what each one changed this turn and this session.
---

**Files** is a project tree in its own column, between the workspace sidebar and your terminals. It shows the folder of the tab you're in, and it shows what your agents are doing in it: a file lights up as an agent reads it, and the files an agent changed carry a mark in that agent's colour, so you can see what a turn touched without scrolling back through its transcript.

## Opening it

Press `Cmd+Shift+B` (`Ctrl+Shift+B` on Windows and Linux), or click the folder button at the top of the sidebar, beside the collapse chevron. While the sidebar is collapsed, the same button sits in the titlebar.

`Cmd+Shift+B` does three things in turn: it opens the column and puts the keyboard in it, puts the keyboard back in it if you've gone elsewhere, and closes it when the tree already has focus. `Esc` in the tree hands the keyboard back to the terminal.

**Only you open, close or resize it.** Nothing in maiTerm does it on your behalf, and an agent can't either. Drag the column's edge to resize it: the width changes once, when you let go, because a width change at a streaming agent makes it redraw, and Claude Code then leaves a duplicate of its transcript in the scrollback.

To turn the feature off, untick **Enable Files** in **Preferences → Files**. That removes the column, its shortcut, its button and the agents' Files tools.

## Which folder it shows

With **Follow the Active Tab** on (the default), the tree shows the folder of the terminal tab in front, and changes when you switch tabs. Inside a git repository it shows the repository and opens the folders down to where the tab is, marking that folder; following never changes what you've selected.

Click the folder's name at the top of the column to choose another: the folder of any terminal tab in the workspace (with its tabs listed), recent folders, or **Open Folder…**. Choosing one pins it for that workspace; the pin button unpins it, and the tree follows the active tab again. **Set as Root (pins)** on a folder's menu does the same from inside the tree.

The **⋯** menu has **Show Hidden Files**, **Show Ignored Files**, **Follow the Active Tab**, **Reveal Active File**, **Reveal This Tab’s Folder** and **Files Settings…**.

A very large folder — your home folder, `/`, a drive root or a mounted volume — is opened in a lighter mode: only the folders you open are watched, git status is off unless the folder is itself a repository, and find doesn't index it until you click **Index this folder**.

### On SSH hosts

An SSH tab's tree is the folder on that host. maiTerm starts a small helper there over the connection the tab already has (its own [bridge tunnel](/features/agents/#ssh-mcp-bridge) when there is one), one per host however many tabs and windows show it, and the tree updates as files change there.

- The helper needs `python3` (3.6 or later) on the host. Without it, the tree still shows the host's files, but read-only, and re-reads open folders every few seconds instead of watching them.
- Without maiTerm's bridge tunnel to the host, the helper needs key-based ssh: a host that asks for a password can't be reached this way, and the tree says so.
- The host has to be a Linux or macOS machine. A Windows host isn't supported.
- **Open With Default App**, **Reveal in Finder** and attaching to the composer are for files on this computer, so they aren't offered on a host's tree. Trashing a file there moves it to `~/.maiterm/trash` on the host, and `Cmd+Z` puts it back.

## Git at a glance

Inside a repository, names are coloured by their git status: modified, added or untracked, deleted, renamed, and conflicted each have their own colour and letter, and a folder holding changed files gets a dot. Hover a changed file for its line counts.

A **branch row** at the top shows the branch, how far ahead and behind its upstream it is, and how many files changed (`⎇ main ↑2 ↓0 · 7 changed`). Click the branch to copy its name; click **N changed** to see only those files.

**Compare with HEAD** on a changed file's menu opens a diff against the last commit. maiTerm only reads git: nothing in Files stages, discards, checks out or commits.

## Finding a file

Press `/` or `Cmd+F` with the tree focused and type. The tree narrows to the files whose paths match, fuzzily, as you type, with their folders open and the matched letters marked. Arrows move through the results, `Enter` opens one, and `Esc` puts the tree back exactly as it was. Files your `.gitignore` excludes aren't searched.

Typing letters in the tree without `/` jumps to the next name that starts with them.

## What your agents are doing

Agent activity comes from the agents' own hooks, so it covers Claude Code and Codex tabs working in the folder the tree shows, local or on an SSH host.

- **Reads light up.** A file an agent reads or searches glows briefly and leaves no lasting mark.
- **Changes stay marked.** A file being edited pulses while the edit runs; afterwards it keeps a dot in the agent's colour for the rest of the turn. A deleted file is struck through. Each agent tab has its own colour, the same one on its chip at the top of the tree.
- **Closed folders count.** A folder that's closed shows how many files inside it agents changed this turn (`2✎`). It never opens by itself.
- **Off-screen activity** shows as a chip on the top or bottom edge (`↑ 3 active`); click it to scroll there.
- **Conflicts are flagged.** A ⚠ marks a file an agent changed while you have unsaved edits to it in an editor tab — click it to compare your buffer with the file on disk — or one two agents changed in the same stretch of time.

Hover a marked file for what happened to it, in words. maiTerm says only what it has evidence for: a file an agent's edit tool named is "edited", but a file that changed on disk while an agent's shell command ran is described as changed *while* that command ran, because a save of yours at that moment would look the same.

### Agent chips and views

Each agent tab working in the folder gets a chip above the tree, with whether it's working or waiting for you and how many files it changed this turn. The view bar under it switches the tree between:

| View | Shows |
|------|-------|
| **All** | The whole folder |
| **Changed** | Files git reports as changed |
| **Turn** | What the agents changed this turn |
| **Session** | What the agents changed this session |

Click a chip to narrow **Turn** and **Session** to that agent. With a chip chosen, **Follow Agent** in the **⋯** menu makes the tree reveal each file that agent writes — but only once you've left the tree alone for three seconds.

### What did that turn change?

On a file an agent changed, **Compare with Turn Start** and **Compare with Session Start** open a diff against what the file held before that agent first edited it, this turn or this session — so you can review exactly what it did, even when the file had other uncommitted changes. With two agents on one file, each gets its own pair, named.

These need **Turn Baselines** in **Preferences → Files** (on by default): before an agent's first edit of a file, maiTerm keeps what it held. That's for files on this computer only, up to 2 MB each, kept in memory — so a restart of maiTerm forgets them.

## Opening files

Click a file and it opens in a **preview tab**, shown in italics, that the next click reuses — so browsing a folder doesn't leave a trail of tabs. Edit the file, or double-click its tab (or the file in the tree), and the tab stays. Turn **Preview Tabs** off in **Preferences → Files** to have every click open a tab of its own.

`Space` previews the focused file without leaving the tree, `Enter` opens it, and `Cmd+Enter` opens it to the side. A file that's already open in an editor tab is brought forward rather than opened twice.

Going the other way, **Reveal in Files** on an editor or terminal tab's right-click menu shows that tab's file or folder in the tree, opening the column if it's closed.

## Working with files

Right-click for **New File…**, **New Folder…**, **Cut**, **Copy**, **Paste**, **Duplicate**, **Rename…**, moving to the trash, **Delete Permanently…**, **Copy Path** and **Copy Relative Path**, **Open Terminal Here**, **Open With Default App** and **Reveal in Finder** (**Show in File Explorer** on Windows). Drag rows onto a folder to move them — hold `Opt` (`Ctrl` on Windows and Linux) to copy — and drop files from Finder or Explorer onto the tree to copy them in.

- **The trash first.** Deleting moves to the Trash (the Recycle Bin on Windows). `Shift+Delete` (`Cmd+Opt+Backspace` on macOS) deletes permanently, and always asks first, naming what will go.
- **Undo.** `Cmd+Z` in the tree undoes the last file operation in that folder — a rename, move, copy, new file or duplicate — and the note at the bottom of the tree offers **Undo** too. Moving to the trash can be undone on Windows, Linux and SSH hosts. On macOS the Trash can't be put back from maiTerm, so the note offers **Show in Trash** instead.
- **Editor tabs follow.** Rename or move a file that's open and its editor tabs follow it. Trashing a file with unsaved changes asks first.
- **Agents at work.** Renaming, moving or deleting a file an agent is changing now, or changed this turn, asks a second time, naming the agent.
- **Folders a tab works in.** A folder that a terminal, an agent or a [stack](/features/stack/) service is working in can't be renamed, moved or trashed from the tree — the agent would recreate the old folder. maiTerm offers **Move Project…** instead, which pauses the agents first and moves everything with it.

## Handing files to an agent

- **Drag files onto a terminal** and their paths go on its command line. Nothing is sent: you read what landed and press `Enter` yourself. A file from this computer dropped on an SSH tab is uploaded to the host first.
- **Drag them onto a [composer](/features/terminal/#composer-dock)** to attach them (files on this computer).
- **Add to ‹tab›** (`Opt+Enter`) puts the selected paths on the line of the tab the tree is following, and **Ask about this…** attaches them to that tab's composer.

Neither ever types into a tab that's showing a permission or trust dialog, where a keypress would answer it.

## Agents can use it too

While Files is enabled, agents get three tools:

| Tool | What it does |
|------|-------------|
| `revealInTree` | Points you at a file or folder in the Files column of the agent's own window: opens the folders down to it and scrolls it into view. It never opens a closed column and never takes the keyboard: if the column is closed, the Files button gets a dot, and if you've used the tree in the last few seconds, a chip naming the path appears instead of moving your view |
| `getTreeSelection` | What you've selected in the tree — paths only, never contents. maiTerm never puts your selection into a prompt by itself, so an agent asks when you say "these files" |
| `getAgentFileActivity` | The files an agent read and changed this turn or this session, as maiTerm recorded them: its own tab's, or another agent tab's in its [mesh](/features/mesh-workspace/) |

## Keys in the tree

| Key | Action |
|-----|--------|
| `↑` `↓` `PgUp` `PgDn` `Home` `End` | Move |
| `→` / `←` | Open a folder or step in / close it or go to its parent |
| `Opt+→` / `Opt+←` | Open / close every folder below |
| `Space` | Preview, keeping focus in the tree |
| `Enter` | Open a file / open or close a folder |
| `Cmd+Enter` | Open to the side |
| `Opt+Enter` | Add the paths to the followed tab (never sent) |
| `Shift+↑` `Shift+↓` · `Cmd+A` | Extend the selection / select all |
| `Cmd+Opt+C` | Copy path (`Cmd+Opt+Shift+C`: relative) |
| `Cmd+Shift+.` | Show or hide hidden files |
| `F2` | Rename |
| `Cmd+Backspace` | Move to the Trash (`Cmd+Opt+Backspace`: delete permanently, asked first) |
| `Cmd+C` / `Cmd+X` / `Cmd+V` | Copy / cut / paste files |
| `Cmd+Z` | Undo the last file operation in this folder |
| `/` or `Cmd+F` | Find a file |
| `Esc` | Close find, then clear the selection, then back to the terminal |

On Windows and Linux, `Cmd` is `Ctrl` and `Opt` is `Alt`, with three differences: copy path is `Shift+Alt+C` (`Ctrl+Shift+Alt+C` for relative), and moving to the trash is `Delete` (`Shift+Delete` to delete permanently).

**Vim Keys** in **Preferences → Files** adds `j` `k` `h` `l`, `gg` `G`, `o`, `y`/`Y` to copy paths and `a` to add to the followed tab; type-ahead then needs a leading `'`.

## Settings

Everything about the view is in **Preferences → Files**: **Follow the Active Tab**, **Reveal the Active File** (open the folders down to the file of the editor tab in front), **Preview Tabs**, **Show Hidden Files**, **Show Ignored Files**, **Git Status**, **Turn Baselines**, **Always Hide** (gitignore patterns, one per line), **Row Density**, **Icons**, **Sticky Folders** (how many parent folders stay pinned at the top while you scroll) and **Vim Keys**.
