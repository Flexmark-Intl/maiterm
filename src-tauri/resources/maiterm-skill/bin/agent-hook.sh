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
#   $2            (optional) the MCP server port baked at install time. Used for the
#                 SSH-remote install, where the reverse-tunnel port is fixed for the
#                 bridge and the live shell may lack $MAITERM_PORT (tmux/sudo/su). Local
#                 installs omit it and rely on the per-process $MAITERM_PORT env var.
#   $MAITERM_PORT  the maiTerm MCP server port (live, per-process)
#   $MAITERM_TAB_ID  the maiTerm tab this Codex session runs in
#
# The ?runtime=codex tag tells maiTerm's /hooks handler to normalize Codex's event
# names/payload; ?tab_id routes the event to the right frontend tab. Output is a bare
# `{}` (a valid no-op decision) so Stop/PreToolUse/PermissionRequest hooks — which
# expect JSON on stdout — get a well-formed "no decision" and never block the turn.

token="$1"
baked_port="${2:-}"

# tmux / sudo / su don't inherit the maiTerm env vars. Fall back to the ~/.aiterm file
# the bridge wrote (export MAITERM_TAB_ID / MAITERM_PORT) so hooks still route correctly.
if [ -z "${MAITERM_TAB_ID:-}" ] || [ -z "${MAITERM_PORT:-}" ]; then
  [ -f "$HOME/.aiterm" ] && . "$HOME/.aiterm" 2>/dev/null || true
fi

# Prefer the install-baked port when present (SSH-remote: the tunnel port is fixed and
# authoritative regardless of the live shell's env); otherwise use the live env port.
port="${baked_port:-${MAITERM_PORT:-}}"
tab="${MAITERM_TAB_ID:-}"

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
