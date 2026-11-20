#!/usr/bin/env bash
# maiTerm Antigravity CLI (agy) hook shim.
#
# Antigravity hooks are COMMAND hooks (no native HTTP hook type) configured inside
# ~/.gemini/config/plugins/maiterm/hooks.json. Each lifecycle event runs this script with
# camelCase protojson on stdin and forwards it to the owning maiTerm instance's /hooks
# endpoint (?runtime=gemini&tab_id=...).
#
# Args / env (set by maiTerm at PTY spawn):
#   --event <Name>   (optional) explicit hook event name passed by hooks.json so untrusted
#                    toolCall.args can never spoof event classification
#   <port>           (optional) MCP server port baked at install time (SSH remote bridge).
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

explicit_event=""
baked_port=""
while [ $# -gt 0 ]; do
  case "$1" in
    --event)
      explicit_event="${2:-}"
      shift 2 2>/dev/null || shift
      ;;
    *)
      baked_port="$1"
      shift
      ;;
  esac
done

# tmux / sudo / su don't inherit maiTerm env vars; fall back to ~/.aiterm when present.
if [ -z "${MAITERM_TAB_ID:-}" ] || [ -z "${MAITERM_PORT:-}" ] || [ -z "${MAITERM_AUTH:-}" ]; then
  [ -f "$HOME/.aiterm" ] && . "$HOME/.aiterm" 2>/dev/null || true
fi

port="${baked_port:-${MAITERM_PORT:-}}"
token="${MAITERM_AUTH:-}"
tab="${MAITERM_TAB_ID:-}"

# Always drain stdin so the pipe never blocks agy.
payload="$(cat)"

# Classify event authoritatively from `--event` (from hooks.json) when present; otherwise fall
# back to payload inspection with PreToolUse checked FIRST so tool args can't spoof it.
event_kind="other"
if [ -n "$explicit_event" ]; then
  case "$explicit_event" in
    PreToolUse) event_kind="pre_tool" ;;
    SessionStart) event_kind="prime_capable" ;;
    PreInvocation)
      case "$payload" in
        *'"invocationNum":0'* | *'"invocationNum": 0'*) event_kind="prime_capable" ;;
        *'"invocationNum"'*) event_kind="other" ;;
        *) event_kind="prime_capable" ;;
      esac
      ;;
  esac
else
  case "$payload" in
    *'"hookEventName":"PreToolUse"'* | *'"hookEventName": "PreToolUse"'*)
      event_kind="pre_tool"
      ;;
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
  esac
fi

reply=""
if [ -n "$port" ] && [ "$port" != "0" ] && [ -n "$token" ]; then
  # Pipe payload over stdin (`--data-binary @-`) rather than argv to avoid Linux 128 KiB E2BIG.
  if [ "$event_kind" = "prime_capable" ]; then
    reply="$(printf '%s' "$payload" | curl -fsS -m 3 \
      -H "Authorization: Bearer ${token}" \
      -H "Content-Type: application/json" \
      --data-binary @- \
      "http://127.0.0.1:${port}/hooks?runtime=gemini&tab_id=${tab}&prime=1&format=agy" \
      2>/dev/null)" || reply=""
  else
    printf '%s' "$payload" | curl -fsS -m 2 \
      -H "Authorization: Bearer ${token}" \
      -H "Content-Type: application/json" \
      --data-binary @- \
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
