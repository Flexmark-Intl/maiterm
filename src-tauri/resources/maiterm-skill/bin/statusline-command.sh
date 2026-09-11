#!/bin/bash
input=$(cat)

# Use the session's base project directory (where Claude Code was launched),
# not the live CWD — subagents change CWD and make the display misleading.
cwd=$(echo "$input" | jq -r '.workspace.project_dir // .cwd // .workspace.current_dir // empty')
[ -z "$cwd" ] && cwd=$(pwd)

# Current git branch (or short commit when detached); empty when not in a repo.
# Run against the real path before $cwd is shortened for display below.
branch=$(git -C "$cwd" symbolic-ref --short -q HEAD 2>/dev/null) \
  || branch=$(git -C "$cwd" rev-parse --short HEAD 2>/dev/null) \
  || branch=""

# Shorten the path: collapse $HOME to ~, then keep only the last 3 components
# (prefixed with … when truncated) so it never dominates the status line.
case "$cwd" in
  "$HOME")   cwd="~" ;;
  "$HOME"/*) cwd="~/${cwd#"$HOME"/}" ;;
esac
IFS='/' read -ra _parts <<< "$cwd"
if [ "${#_parts[@]}" -gt 3 ]; then
  _start=$(( ${#_parts[@]} - 3 ))
  cwd="…/$(IFS='/'; echo "${_parts[*]:_start}")"
fi

model=$(echo "$input" | jq -r '.model.display_name // empty')
model_id=$(echo "$input" | jq -r '.model.id // empty')
effort=$(echo "$input" | jq -r '.effort.level // empty')

# Prefer the official model id (e.g. opus-5[1m]) over the friendly display name,
# falling back to the display name if the id isn't present.
model_label="${model_id#claude-}"
[ -z "$model_label" ] && model_label="$model"
transcript=$(echo "$input" | jq -r '.transcript_path // empty')

ctx_tokens=0
if [ -n "$transcript" ] && [ -f "$transcript" ]; then
  ctx_tokens=$(tac "$transcript" 2>/dev/null | awk '
    /"usage"/ {
      print
      exit
    }
  ' | jq -r '
    .message.usage // empty |
    ((.input_tokens // 0) + (.cache_read_input_tokens // 0) + (.cache_creation_input_tokens // 0))
  ' 2>/dev/null)
  [ -z "$ctx_tokens" ] && ctx_tokens=0
fi

host=$(hostname -s 2>/dev/null | tr '[:lower:]' '[:upper:]')
[ -z "$host" ] && host="HOST"
# Keep the host label from dominating the line: cap at 12 chars,
# keeping 11 plus an ellipsis so truncation is visible.
[ "${#host}" -gt 12 ] && host="${host:0:11}…"

# Context window for this model. THREE tests, in falling order of confidence — the same
# ladder as `context_limit_for` in src-tauri/src/mailink/mod.rs, which solved this first.
# Keep the two in step: they are separate code paths reading different inputs, which is
# exactly why this one sat wrong after the other was fixed.
#
#   1. The explicit variant marker. Claude Code puts it on `.model.id` in the statusLine
#      input, so for Claude this is authoritative and nothing below runs.
#   2. Model families known to be 1M with no marker. Other runtimes carry no marker at all:
#      `fable-5-1` matched neither pattern, fell through to 200k, and rendered a real
#      session as "182.2%" — which is not a percentage of anything.
#   3. Observed usage as a self-correcting backstop. A session already past 200k is
#      definitively on a bigger window whatever its id says. This is the part that makes
#      the next unrecognised 1M model merely wrong-until-200k instead of permanently
#      pegged over 100%, with no code change per model.
case "$model_id" in
  *"[1m]"*|*"-1m"*) ctx_limit=1000000 ;;
  *"opus-4-8"*|*"opus-5"*|*"fable-5"*) ctx_limit=1000000 ;;
  *) ctx_limit=200000 ;;
esac
# Compared through awk, not `[ -gt ]`, so a non-integer token count can never make this
# render an error instead of a status line. Non-numeric reads as 0 there and the backstop
# simply doesn't trigger, which is the right way to fail.
if [ "$ctx_limit" -eq 200000 ] && awk -v t="$ctx_tokens" 'BEGIN{exit !(t>200000)}'; then
  ctx_limit=1000000
fi
pct=$(awk -v t="$ctx_tokens" -v l="$ctx_limit" 'BEGIN{printf "%.1f", (t/l)*100}')

GREEN='\033[1;32m'
CYAN='\033[1;36m'
BLUE='\033[1;34m'
MAGENTA='\033[1;35m'
YELLOW='\033[1;33m'
ORANGE='\033[38;5;208m'
RESET='\033[0m'

printf '%b%s %s%b[%s]%b' "$GREEN" "$host" "$(whoami)" "$CYAN" "$cwd" "$RESET"
[ -n "$branch" ] && printf ' %b(%s)%b' "$ORANGE" "$branch" "$RESET"
[ -n "$model_label" ] && printf ' %b%s%b' "$BLUE" "$model_label" "$RESET"
[ -n "$effort" ] && printf ' %b%s%b' "$YELLOW" "$effort" "$RESET"
printf ' %b%s%%%b' "$MAGENTA" "$pct" "$RESET"
