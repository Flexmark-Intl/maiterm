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
- **Cmd+N** — new workspace
- **Cmd+,** — open preferences
- **Cmd+/** — help and keyboard shortcuts

## Setting Up Agent Integration

maiTerm integrates with **Claude Code** and **OpenAI Codex**, both enabled by default. When you run `claude` or `codex` in a terminal tab, maiTerm automatically:

1. Exposes MCP/IDE tools to the agent
2. Tracks live state through the agent's hooks
3. Captures the session ID and enables auto-resume

No configuration needed — it just works, and maiTerm detects which agent connected on its own (no manual `/maiterm init`). To choose which agents maiTerm wires up — locally and over SSH — open **Preferences → AI Agents**. See [Agent Integration](/features/claude-code/) for the full picture.
