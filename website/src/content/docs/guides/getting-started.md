---
title: Getting Started
description: Install maiTerm and get up and running on macOS, Windows, or Linux.
---

## Download

Download the latest release from the [GitHub Releases page](https://github.com/Flexmark-Intl/maiterm/releases). After installing, maiTerm checks for updates automatically and notifies you when a new version is available — update with a single click.

| Platform | Format |
|----------|--------|
| macOS | DMG |
| Windows | NSIS installer (.exe) |
| Linux | .deb package |

## Prerequisites (Building from Source)

If you want to build maiTerm from source, you'll need:

**All platforms:**
- [Node.js](https://nodejs.org/) 18+
- [Rust](https://rustup.rs/)

**macOS:**
- macOS 13+
- Xcode Command Line Tools (`xcode-select --install`)

**Windows:**
- Windows 10/11
- [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) — select "Desktop development with C++" workload
- [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (pre-installed on Windows 10/11)

**Linux:**
- WebKitGTK 4.1, GTK 3, libayatana-appindicator3
- See [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux)

## First Launch

1. Open maiTerm
2. You'll start with a default workspace and a single terminal tab
3. The terminal spawns your default shell automatically

## Quick Tips

- **Cmd+T** — new tab
- **Cmd+D** — split pane (duplicates current tab with full context)
- **Cmd+Opt+N** — new workspace
- **Cmd+E** — notes panel · **Cmd+Shift+E** — [task panel](/features/tasks/)
- **Cmd+Shift+B** — [Files](/features/files/), the project tree that shows what your agents read and change
- **Cmd+,** — open preferences
- **Cmd+/** — help and keyboard shortcuts

## Setting Up Agent Integration

maiTerm integrates with **Claude Code** and **OpenAI Codex**, both enabled by default. When you run `claude` or `codex` in a terminal tab, maiTerm automatically:

1. Exposes MCP/IDE tools to the agent
2. Tracks live state through the agent's hooks
3. Captures the session ID and enables auto-resume
4. Binds the session to the tab it's running in

No configuration needed, and **no registration step**: the tab identifies itself on the wire and the session links itself when the agent starts, so a restart, a resume or a compaction all come back correctly bound with nothing typed. (`/maiterm init` still exists, but only as a repair tool — see [acting as the right tab](/features/agents/#acting-as-the-right-tab).)

To choose which agents maiTerm wires up — locally and over SSH — open **Preferences → AI Agents**. See [Agent Integration](/features/agents/) for the full picture.

### Claude Code on Windows

Claude Code uses Git Bash to run its commands, and maiTerm's hooks, the part that tells maiTerm which tab a Claude session is in, are shell scripts that run the same way. Without Git for Windows, Claude falls back to PowerShell and maiTerm's tab tracking, tasks and [Overlord](/features/overlord/) can't reach your Claude sessions.

The **Set up Claude on Windows** wizard checks this for you. It opens by itself once, on the first launch that finds Git for Windows or Claude Code missing, and any time from **Preferences → Default Shell → Check Claude setup…**. It checks for:

- **Git for Windows**, wherever it's installed. **Install** runs it through winget in a visible tab, or opens the download page when winget isn't there
- **The shell for new tabs**, with a **Use Git Bash** button when Git is installed but tabs use something else
- **Claude Code**. **Install** runs Anthropic's official installer in a visible tab
- **Whether `claude` is on your PATH.** Its installer doesn't add it. maiTerm's own tabs find it anyway; **Add to PATH** puts it on your user PATH so other terminals do too
- **An older Claude Desktop that takes over `claude`**, so typing it opens the desktop app. The wizard tells you how to turn that alias off
- **WSL**, which it reports but doesn't set up: Claude inside WSL is a separate install, and maiTerm's Claude integration is set up for Windows itself

**Fix everything** does whatever it can at once. When nothing is missing, **Start Claude in a new tab** takes you straight in.

The default shell is **Automatic (Git Bash when installed)**: Git Bash when Git for Windows is there, PowerShell when it isn't. If you'd picked a shell before this setting existed, you keep it. Change it under **Preferences → Default Shell**; it applies to new tabs.

Windows support for Claude is new and has had less real-world use than macOS. The [Claude Code mod](/features/agents/#the-maiterm-mod-claude-code) isn't loaded on Windows yet, so Claude tabs there run on the settings hooks alone.

Your agent also shares a [kanban board](/features/tasks/) with you out of the box: it records what it's working on, you edit the same rows in a panel on `Cmd+Shift+E`, and each side sees the other's changes.

## Let the workspace run your project

Right-click a workspace in the sidebar and choose **Import from project…**. maiTerm reads what the directory already declares — `package.json` scripts, a Procfile, a compose file, a justfile or a Makefile — and offers them as a checklist. Tick the ones that should come up with the workspace and they become part of its [stack](/features/stack/): started in tabs maiTerm owns, watched in a console drawer, restarted when they crash, and visible to your agents so they stop starting a second copy of a dev server that's already running.

When a service announces where it's serving, maiTerm reads the address off its own output and puts it on the row — nothing to configure. Shift-click the row, or use the `↗` on it, to open that in your browser.
