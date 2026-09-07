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
#
# SessionStart is the ONE event whose reply is used. It asks for `prime=1&format=codex`
# and prints the answer, which is maiTerm's standing instructions (tasks + Overlord) for
# this tab, already wrapped in Codex's SessionStart output shape. The server does that
# wrapping so nothing here has to JSON-escape a multi-line string in bash. This is how the
# instructions reach an agent that never calls initSession — including a resumed one,
# which takes no turn until its human types.

token="$1"
baked_port="${2:-}"

# tmux / sudo / su don't inherit the maiTerm env vars. Fall back to the ~/.aiterm file
# the bridge wrote (export MAITERM_TAB_ID / MAITERM_PORT) so hooks still route correctly.
# Setup suppresses this file when this maiTerm sees multiple bridged tabs. Other
# instances' tabs are invisible to that gate; cross-instance fallback remains unsafe
# (docs/codex-integration-review.md C1).
if [ -z "${MAITERM_TAB_ID:-}" ] || [ -z "${MAITERM_PORT:-}" ]; then
  [ -f "$HOME/.aiterm" ] && . "$HOME/.aiterm" 2>/dev/null || true
fi

# Prefer the install-baked port when present (SSH-remote: the tunnel port is fixed and
# authoritative regardless of the live shell's env); otherwise use the live env port.
port="${baked_port:-${MAITERM_PORT:-}}"
tab="${MAITERM_TAB_ID:-}"

# Read the event payload from stdin regardless, so the pipe never blocks Codex.
payload="$(cat)"

# Only SessionStart asks for a reply. Matched on the raw payload rather than parsed: this
# runs on every hook of every turn, and jq/python are not guaranteed to exist on a remote.
# Both spacings are matched because the separator is a serializer detail, not a contract.
case "$payload" in
  *'"hook_event_name":"SessionStart"'* | *'"hook_event_name": "SessionStart"'*) want_reply=1 ;;
  *) want_reply=0 ;;
esac

reply=""
if [ -n "$port" ]; then
  if [ "$want_reply" = 1 ]; then
    # -m 3 leaves headroom inside the 5s timeout SessionStart is registered with.
    reply="$(curl -fsS -m 3 \
      -H "Authorization: Bearer ${token}" \
      -H "Content-Type: application/json" \
      --data-binary "$payload" \
      "http://127.0.0.1:${port}/hooks?runtime=codex&tab_id=${tab}&prime=1&format=codex" \
      2>/dev/null)" || reply=""
  else
    curl -fsS -m 2 \
      -H "Authorization: Bearer ${token}" \
      -H "Content-Type: application/json" \
      --data-binary "$payload" \
      "http://127.0.0.1:${port}/hooks?runtime=codex&tab_id=${tab}" \
      >/dev/null 2>&1 || true
  fi
fi

# Print the server's document when we got one, otherwise a valid empty decision: don't
# continue (Stop), don't block (Pre*). maiTerm only observes Codex; it never drives
# continuation via the hook return. An unreachable server, an unknown tab or a timeout all
# land here, so a broken maiTerm degrades to "no priming", never to a malformed hook reply.
case "$reply" in
  '{'*) printf '%s' "$reply" ;;
  *) printf '%s' '{}' ;;
esac
exit 0
