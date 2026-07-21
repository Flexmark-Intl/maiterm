---
name: maiterm
description: Quick maiTerm terminal operations — /maiterm notes, /maiterm diag, /maiterm tabs, etc.
---

## Fast path: `init`

If the argument is `init`, do ONLY this and stop — do NOT read the command table below and do NOT keyword-search across MCP servers:
1. Load the tool with one targeted lookup: ToolSearch `select:mcp__maiterm__initSession,mcp__maiterm-dev__initSession,mcp__aiterm__initSession,mcp__aiterm-dev__initSession`
2. Call the one named in your SessionStart hook context (this build registers exactly one of maiterm/maiterm-dev; the aiterm/aiterm-dev names are legacy fallbacks) with `{ "tabId": "<value of $MAITERM_TAB_ID>", "sessionId": "<from your SessionStart hook context>" }`.

Always re-run this when asked, even if you think you already initialized — resume/fork/compact require re-init.

Execute the maiTerm MCP tool for the requested operation. Use whichever maiterm MCP server you already called initSession on (maiterm or maiterm-dev). If you haven't initialized yet, call initSession first.

## Command reference

| Command | MCP Tool | Parameters |
|---------|----------|------------|
| `notes` | openNotesPanel | `{ "open": true }` |
| `notes close` | openNotesPanel | `{ "open": false }` |
| `notes read` | getTabNotes | `{}` |
| `notes write <content>` | setTabNotes | `{ "notes": "<content>" }` |
| `notes edit <old> <new>` | editTabNotes | `{ "old_string": "<old>", "new_string": "<new>" }` |
| `tabs` | listWorkspaces | `{}` |
| `tab` | getActiveTab | `{}` |
| `switch <tabId>` | switchTab | `{ "tabId": "<tabId>" }` |
| `open <filePath>` | openFile | `{ "filePath": "<filePath>" }` |
| `windows` | listWindows | `{}` |
| `diag` | getDiagnostics | `{}` |
| `vars` | getTriggerVariables | `{}` |
| `var <name> <value>` | setTriggerVariable | `{ "name": "<name>", "value": "<value>" }` |
| `resume on` | setAutoResume | `{ "enabled": true }` |
| `resume off` | setAutoResume | `{ "enabled": false }` |
| `resume` | getAutoResume | `{}` |
| `archived` | listArchivedTabs | `{}` |
| `restore <tabId>` | restoreArchivedTab | `{ "tabId": "<tabId>" }` |
| `prefs` | getPreferences | `{}` |
| `prefs <query>` | getPreferences | `{ "query": "<query>" }` |
| `backup` | createBackup | `{}` |
| `notify <title> <body>` | sendNotification | `{ "title": "<title>", "body": "<body>" }` |
| `logs` | readLogs | `{}` |
| `logs <search>` | readLogs | `{ "search": "<search>" }` |
| `sessions` | getClaudeSessions | `{}` |
| `reply <text>` | postCommsReply | `{ "message": "<text>" }` |
| `unbind` | unbindCommsThread | `{}` |
| `init` | initSession | `{ "tabId": "$MAITERM_TAB_ID", "sessionId": "<from SessionStart hook>" }` |

Call the exact MCP tool listed above with the specified parameters. Do not ask for clarification — just execute.
For `init`: read tabId from $MAITERM_TAB_ID env var and sessionId from your SessionStart hook context. IMPORTANT: Always call initSession when requested, even if you believe it was already called earlier in the session. Session resume, fork, and compact events require re-initialization to pick up state changes.

## statusline — install the maiTerm status line

`statusline` is the one subcommand that is NOT an MCP tool. It installs the maiTerm-recommended Claude Code status line (host · cwd · git branch · model · effort · context-used %) on THIS machine. Be fast and minimal:

1. Run: `bash ~/.claude/skills/maiterm/bin/setup-statusline.sh`
2. The script prints a real colored example, then signals via its exit code:
   - exit 0 → installed. In one short sentence, tell the user it's active in new Claude Code sessions and stop. Do not re-echo the example.
   - exit 3 → jq is missing (needed to parse Claude's JSON and merge settings). The script printed a line `JQ_MISSING:<install command>`. Show that command and ask whether to run it; if yes, run it then re-run the setup script; if no, stop and explain it can't install without jq.
   - any other non-zero → show the script's output and stop.

The install is idempotent — it only writes `~/.claude/statusline-command.sh` and sets the `statusLine` key in `~/.claude/settings.json`, preserving other keys.

## resolve — work a Mattermost thread as a bug report

`resolve <permalink>` binds this tab to a Mattermost thread and works it to resolution:

1. Call bindCommsThread `{ "url": "<permalink>" }`. The result contains the full thread as a transcript — `[REPORT]` marks the root post, usually a bug report relayed by support staff on behalf of a customer.
2. Investigate and fix the issue in this tab's repository. While working, stay SILENT on the thread — no progress updates. Exception: if you genuinely cannot proceed without more information, ask ONE concise question via postCommsReply (without the `resolve` flag), and address it explicitly to the right audience — start the message with `**@Support:**` (questions about what the customer saw/did, repro details) or `**@Dev:**` (questions about the codebase, environment, or release process) — so the humans in the channel know who should answer. The answer arrives in this session automatically.
3. While bound, messages beginning `[Mattermost thread update …]` may appear in this session: they are new replies from humans in the thread. Treat them as steering input. Do not acknowledge each one on the thread.
4. When the issue is fixed and verified, post the resolution: postCommsReply `{ "message": "<formatted below>", "resolve": true }`. This closes the binding. If you abandon the issue instead, post a brief note saying so via postCommsReply, then call unbindCommsThread `{}`.

Resolution post format (Mattermost markdown), exactly two parts:

- **Part 1 — for support staff.** 2–4 plain-language sentences addressed to the support person who relayed the report: what the customer was experiencing, what was wrong (no jargon, no file names, no code), what changes for the customer and when (e.g. "fixed in the next release"), and anything they should pass along to the customer.
- A `---` separator line.
- **Part 2** — starts with `**Technical details (for devs):**` followed by bullets: root cause, files/functions changed, how it was verified, any follow-ups.

If bindCommsThread reports the integration is not configured, tell the user to set the server URL and bot token in maiTerm Preferences → Integrations and stop.

$ARGUMENTS
