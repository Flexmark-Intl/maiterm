#!/usr/bin/env bash
# maiTerm Antigravity CLI (agy) hook shim.
#
# Antigravity hooks are COMMAND hooks (no native HTTP hook type) configured inside
# ~/.gemini/config/plugins/maiterm/hooks.json. Each lifecycle event runs this script with
# camelCase protojson on stdin and forwards it to the owning maiTerm instance's /hooks
# endpoint (?runtime=gemini&tab_id=...).
#
# Args / env (set by maiTerm at PTY spawn):
#   $1               (optional) MCP server port baked at install time (SSH remote bridge).
#                    Local installs omit it and read the per-process $MAITERM_PORT so dev
#                    and prod share one plugin definition cleanly.
#   $MAITERM_PORT    the owning maiTerm instance's MCP server port
#   $MAITERM_AUTH    the MCP auth token
#   $MAITERM_TAB_ID  the maiTerm tab this agy session runs in
#
# Output contract (verified live on agy 1.5.13, docs/agy-integration-review.md):
#   - PreToolUse: MUST emit empty stdout for observational pass-through. Emitting `{}`
#     causes agy to HARD DENY the tool call, and `{"decision":"allow"}` would bypass the
#     user's permission policy.
#   - SessionStart & PreInvocation: ask for `prime=1&format=agy` and print the server's
#     `{"injectSteps":…}` JSON (or `{}` when no priming/context is needed). `PreInvocation`
#     primes resumed `agy --conversation` sessions (where SessionStart does not fire) once
#     and delivers Browser Link tab updates on turn 0.
#   - All other events (PostInvocation, PostToolUse, Stop): print `{}` so
#     Stop never blocks/continues and Post* gets valid JSON.

baked_port="${1:-}"

# tmux / sudo / su don't inherit maiTerm env vars; fall back to ~/.aiterm when present.
if [ -z "${MAITERM_TAB_ID:-}" ] || [ -z "${MAITERM_PORT:-}" ] || [ -z "${MAITERM_AUTH:-}" ]; then
  [ -f "$HOME/.aiterm" ] && . "$HOME/.aiterm" 2>/dev/null || true
fi

port="${baked_port:-${MAITERM_PORT:-}}"
token="${MAITERM_AUTH:-}"
tab="${MAITERM_TAB_ID:-}"

# Always drain stdin so the pipe never blocks agy.
payload="$(cat)"

# Classify event on the raw JSON payload (no jq/python dependency).
event_kind="other"
case "$payload" in
  *'"hookEventName":"SessionStart"'* | *'"hookEventName": "SessionStart"'*)
    event_kind="prime_capable"
    ;;
  *'"hookEventName":"PreInvocation"'* | *'"hookEventName": "PreInvocation"'*)
    # Only ask for priming / turn context on invocationNum == 0 (protojson omits zero fields).
    case "$payload" in
      *'"invocationNum":0'* | *'"invocationNum": 0'*) event_kind="prime_capable" ;;
      *'"invocationNum"'*) event_kind="other" ;;
      *) event_kind="prime_capable" ;;
    esac
    ;;
  *'"hookEventName":"PreToolUse"'* | *'"hookEventName": "PreToolUse"'*)
    event_kind="pre_tool"
    ;;
esac

reply=""
if [ -n "$port" ] && [ "$port" != "0" ] && [ -n "$token" ]; then
  if [ "$event_kind" = "prime_capable" ]; then
    reply="$(curl -fsS -m 3 \
      -H "Authorization: Bearer ${token}" \
      -H "Content-Type: application/json" \
      --data-binary "$payload" \
      "http://127.0.0.1:${port}/hooks?runtime=gemini&tab_id=${tab}&prime=1&format=agy" \
      2>/dev/null)" || reply=""
  else
    curl -fsS -m 2 \
      -H "Authorization: Bearer ${token}" \
      -H "Content-Type: application/json" \
      --data-binary "$payload" \
      "http://127.0.0.1:${port}/hooks?runtime=gemini&tab_id=${tab}" \
      >/dev/null 2>&1 || true
  fi
fi

if [ "$event_kind" = "pre_tool" ]; then
  # Empty stdout = observational pass-through (never deny, never bypass permissions).
  exit 0
fi

case "$reply" in
  '{'*) printf '%s' "$reply" ;;
  *) printf '%s' '{}' ;;
esac
exit 0
