#!/bin/sh
# maiTerm Hive hook (docs/hive.md §6.8, §11.1). maiTerm writes this file at
# <app data>/hive/bin/ on every start; it is on every Hive agent's denyWrite.
#
# Every hook in a Hive tab is a command hook: the sandbox breaks localhost http
# hooks, and a command hook runs outside it (spike 9).
#
#   maiterm-hive-hook gate            PreToolUse on Bash/PowerShell/Write/Edit.
#                                     Exit 0 only on maiTerm's exact "allow"; any
#                                     other outcome (maiTerm down, slow, non-2xx,
#                                     a deny) is exit 2 with the reason on stderr.
#   maiterm-hive-hook observe EVENT   Everything else: forwarded, never blocks.
#                                     Only SessionStart and PreModelSwitch print the
#                                     reply, which Claude reads as hook output.
#
# The port is read from ./port, which maiTerm rewrites on every bind, so a restart
# can't strand a tab; $MAITERM_PORT is the fallback. Nothing secret is in this file
# or on its command line: the token comes from the tab's environment.

mode="$1"
event="$2"
dir=$(dirname "$0")
port=$(cat "$dir/port" 2>/dev/null)
case "$port" in
  '' | *[!0-9]*) port="$MAITERM_PORT" ;;
esac
input=$(cat)
base="http://127.0.0.1:$port"
auth="x-claude-code-ide-authorization: $MAITERM_AUTH"

if [ "$mode" = gate ]; then
  out=$(printf '%s' "$input" | curl -s -f --connect-timeout 2 --max-time 8 \
    -H "$auth" -H 'content-type: application/json' --data-binary @- \
    "$base/hive/gate?tab_id=$MAITERM_TAB_ID" 2>/dev/null)
  status=$?
  if [ "$status" -eq 0 ] && [ "$out" = allow ]; then
    exit 0
  fi
  case "$out" in
    deny:*) printf '%s\n' "${out#deny:}" >&2 ;;
    *) echo "maiTerm could not check this call, so it is blocked: the Hive gate fails closed. Try again in a moment; if it keeps failing, tell your coordinator or human." >&2 ;;
  esac
  exit 2
fi

query="tab_id=$MAITERM_TAB_ID&via=hive"
[ "$event" = SessionStart ] && query="$query&prime=1"
out=$(printf '%s' "$input" | curl -s -f --connect-timeout 2 --max-time 4 \
  -H "$auth" -H 'content-type: application/json' --data-binary @- \
  "$base/hooks?$query" 2>/dev/null)
case "$event" in
  SessionStart | PreModelSwitch) [ -n "$out" ] && printf '%s\n' "$out" ;;
esac
exit 0
