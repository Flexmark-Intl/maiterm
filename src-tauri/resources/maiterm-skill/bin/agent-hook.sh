#!/usr/bin/env bash
# maiTerm Codex hook shim.
#
# Codex hooks are COMMAND hooks (no native HTTP hook type), so each lifecycle event
# runs this script, which forwards the event to the local maiTerm MCP server's /hooks
# endpoint — the same endpoint Claude Code posts to over HTTP. Codex passes the hook
# event as JSON on stdin.
#
# Args / env (set by maiTerm at install + PTY spawn):
#   $1            the MCP auth token (embedded in ~/.codex/hooks.json by CodexRegistrar)
#   $AITERM_PORT  the maiTerm MCP server port
#   $AITERM_TAB_ID  the maiTerm tab this Codex session runs in
#
# The ?runtime=codex tag tells maiTerm's /hooks handler to normalize Codex's event
# names/payload; ?tab_id routes the event to the right frontend tab. Output is a bare
# `{}` (a valid no-op decision) so Stop/PreToolUse/PermissionRequest hooks — which
# expect JSON on stdout — get a well-formed "no decision" and never block the turn.

token="$1"
port="${AITERM_PORT:-}"
tab="${AITERM_TAB_ID:-}"

# Read the event payload from stdin regardless, so the pipe never blocks Codex.
payload="$(cat)"

if [ -n "$port" ]; then
  curl -fsS -m 2 \
    -H "Authorization: Bearer ${token}" \
    -H "Content-Type: application/json" \
    --data-binary "$payload" \
    "http://127.0.0.1:${port}/hooks?runtime=codex&tab_id=${tab}" \
    >/dev/null 2>&1 || true
fi

# A valid empty decision: don't continue (Stop), don't block (Pre*). maiTerm only
# observes Codex; it never drives continuation via the hook return.
printf '%s' '{}'
exit 0
