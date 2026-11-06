---
name: maiterm-statusline
description: Install maiTerm's Claude Code status line (host · cwd · git branch · model · effort · context-used %) on this machine. Not Claude Code's built-in /statusline, which copies your shell prompt.
---

Install the maiTerm-recommended Claude Code status line on THIS machine (local, or the SSH host this session runs on). Be fast and minimal:

1. Run: `bash ~/.claude/skills/maiterm-statusline/bin/setup-statusline.sh`
2. The script prints a real colored example, then signals via its exit code:
   - exit 0 → installed. In one short sentence, tell the user it's active in new Claude Code sessions and stop. Do not re-echo the example.
   - exit 3 → jq is missing (needed to parse Claude's JSON and merge settings). The script printed a line `JQ_MISSING:<install command>`. Show that command and ask whether to run it; if yes, run it then re-run the setup script; if no, stop and explain it can't install without jq.
   - any other non-zero → show the script's output and stop.

The install is idempotent — it only writes `~/.claude/statusline-command.sh` and sets the `statusLine` key in `~/.claude/settings.json`, preserving other keys.
