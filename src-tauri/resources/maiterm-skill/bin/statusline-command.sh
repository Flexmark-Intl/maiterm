#!/bin/bash
input=$(cat)

cwd=$(echo "$input" | jq -r '.cwd // .workspace.current_dir // empty')
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

# Prefer the official model id (e.g. opus-4-8[1m]) over the friendly display name,
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

case "$model_id" in
  *"[1m]"*|*"-1m"*) ctx_limit=1000000 ;;
  *) ctx_limit=200000 ;;
esac
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
