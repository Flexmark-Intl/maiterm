---
name: maiterm-statusline
description: Install maiTerm's status line for Claude Code and Antigravity CLI (agy) (host · cwd · git branch · model · effort · context-used %) on this machine. Replaces the built-in status line.
---

Install the maiTerm-recommended status line for Claude Code and Antigravity CLI (`agy`) on THIS machine (local, or the SSH host this session runs on). Be fast and minimal:

1. Run `setup-statusline.sh` from this skill's `bin/` directory:
   - In Antigravity (`agy`): `bash ~/.gemini/config/plugins/maiterm/skills/maiterm-statusline/bin/setup-statusline.sh`
   - In Claude Code: `bash ~/.claude/skills/maiterm-statusline/bin/setup-statusline.sh`
2. The script prints a real colored example, then signals via its exit code:
   - exit 0 → installed. In one short sentence, tell the user which runtimes it configured and that it's active in new sessions, then stop. Do not re-echo the example.
   - exit 3 → jq is missing (needed to parse the status line JSON and merge settings). The script printed a line `JQ_MISSING:<install command>`. Show that command and ask whether to run it; if yes, run it then re-run the setup script; if no, stop and explain it can't install without jq.
   - any other non-zero → show the script's output and stop.

The install is idempotent — for each detected runtime it only writes `statusline-command.sh` and merges the `statusLine` key into `settings.json` (`~/.claude/settings.json` for Claude Code; `~/.gemini/antigravity-cli/settings.json` with `stack_with_default: false` for `agy`), preserving all other keys.
