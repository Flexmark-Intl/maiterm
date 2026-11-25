#!/usr/bin/env bash
# maiTerm status line — local installer for Claude Code and Antigravity CLI (agy).
#
# Optimized for the happy path: when jq is already present this does the whole
# job in one shot (render example → copy script → merge settings.json) with no
# prompts. It NEVER installs jq itself — if jq is missing it prints the exact
# command for the detected package manager and exits 3 so the caller (the
# /maiterm-statusline skill) can offer to run it interactively.
#
# Exit codes:
#   0  installed
#   3  jq missing — a line "JQ_MISSING:<install command>" was printed
#   1  unexpected error
set -e

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAYLOAD="$SKILL_DIR/statusline-command.sh"
CLAUDE_DIR="$HOME/.claude"
CLAUDE_DEST="$CLAUDE_DIR/statusline-command.sh"
CLAUDE_SETTINGS="$CLAUDE_DIR/settings.json"
AGY_DIR="$HOME/.gemini/antigravity-cli"
AGY_DEST="$AGY_DIR/statusline-command.sh"
AGY_SETTINGS="$AGY_DIR/settings.json"

[ -f "$PAYLOAD" ] || { echo "error: bundled statusline-command.sh not found at $PAYLOAD" >&2; exit 1; }

# --- Render a representative example using REAL colors + this machine's values,
#     so the user sees exactly what their status line will look like. ---
GREEN='\033[1;32m'; CYAN='\033[1;36m'; BLUE='\033[1;34m'
MAGENTA='\033[1;35m'; YELLOW='\033[1;33m'; ORANGE='\033[38;5;208m'; RESET='\033[0m'
host=$(hostname -s 2>/dev/null | tr '[:lower:]' '[:upper:]'); [ -z "$host" ] && host="HOST"
[ "${#host}" -gt 12 ] && host="${host:0:11}…"  # match statusline-command.sh truncation

echo "maiTerm status line — example:"
echo
printf '  %b%s %s%b[%s]%b %b(%s)%b %b%s%b %b%s%b %b%s%%%b\n' \
  "$GREEN" "$host" "$(whoami)" \
  "$CYAN" "~/projects/maiterm" "$RESET" \
  "$ORANGE" "main" "$RESET" \
  "$BLUE" "opus-5-5" "$RESET" \
  "$YELLOW" "high" "$RESET" \
  "$MAGENTA" "12.3" "$RESET"
echo
echo "  host+user · [cwd] · (git branch) · model · effort · context-used%"
echo

# --- jq gate: required to parse statusLine JSON at runtime AND to merge settings.
#     Detect-and-report only; do not install. ---
if ! command -v jq >/dev/null 2>&1; then
  cmd="(install jq with your package manager, then re-run)"
  if   command -v apt-get >/dev/null 2>&1; then cmd="sudo apt-get update && sudo apt-get install -y jq"
  elif command -v dnf     >/dev/null 2>&1; then cmd="sudo dnf install -y jq"
  elif command -v yum     >/dev/null 2>&1; then cmd="sudo yum install -y jq"
  elif command -v apk     >/dev/null 2>&1; then cmd="sudo apk add jq"
  elif command -v brew    >/dev/null 2>&1; then cmd="brew install jq"
  fi
  echo "JQ_MISSING:$cmd"
  exit 3
fi

cmd_for() {
  case "$1" in
    *" "*|*"'"*) printf "bash '%s'" "${1//\'/\'\\\'\'}" ;;
    *) printf "bash %s" "$1" ;;
  esac
}

merge_json() {
  local file="$1"
  local filter="$2"
  shift 2
  local target="$file"
  if [ -L "$file" ]; then
    target=$(readlink -f "$file" 2>/dev/null \
      || python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$file" 2>/dev/null \
      || echo "$file")
  fi
  mkdir -p "$(dirname "$target")"
  [ -f "$target" ] || printf '{}\n' > "$target"
  if ! jq -e 'type == "object"' "$target" >/dev/null 2>&1; then
    echo "error: refusing to modify $file (not a valid JSON object)" >&2
    return 1
  fi
  local tmp
  tmp=$(mktemp "$(dirname "$target")/.statusline-settings.XXXXXX")
  if jq "$@" "$filter" "$target" > "$tmp"; then
    mv "$tmp" "$target"
  else
    rm -f "$tmp"
    return 1
  fi
}

# --- Detect which runtimes to configure (Claude Code, Antigravity CLI, or both). ---
install_claude=0
install_agy=0

if [ -d "$CLAUDE_DIR" ] || command -v claude >/dev/null 2>&1; then
  install_claude=1
fi
if [ -d "$AGY_DIR" ] || command -v agy >/dev/null 2>&1 \
   || [ -x "$HOME/.local/bin/agy" ] || [ -x "$HOME/.antigravity/bin/agy" ]; then
  install_agy=1
fi

# Always configure the runtime whose skill directory launched this script.
case "$SKILL_DIR" in
  "$HOME/.gemini"/*) install_agy=1 ;;
  "$HOME/.claude"/*) install_claude=1 ;;
esac

if [ "$install_claude" -eq 0 ] && [ "$install_agy" -eq 0 ]; then
  install_claude=1
fi

installed=()

# --- Install: copy the bundled script + merge the statusLine key into
#     settings.json, preserving every other key. Idempotent. ---
if [ "$install_claude" -eq 1 ]; then
  mkdir -p "$CLAUDE_DIR"
  install -m 0755 "$PAYLOAD" "$CLAUDE_DEST"
  merge_json "$CLAUDE_SETTINGS" \
    '.statusLine = {type:"command", command:$cmd}' \
    --arg cmd "$(cmd_for "$CLAUDE_DEST")"
  echo "Installed Claude Code → $CLAUDE_DEST"
  installed+=("Claude Code")
fi

if [ "$install_agy" -eq 1 ]; then
  mkdir -p "$AGY_DIR"
  install -m 0755 "$PAYLOAD" "$AGY_DEST"
  merge_json "$AGY_SETTINGS" \
    '.statusLine = {type:"command", command:$cmd, enabled:true, stack_with_default:false}' \
    --arg cmd "$(cmd_for "$AGY_DEST")"
  echo "Installed Antigravity (agy) → $AGY_DEST"
  installed+=("Antigravity (agy)")
fi

if [ "${#installed[@]}" -eq 2 ]; then
  runtimes="${installed[0]} and ${installed[1]}"
else
  runtimes="${installed[0]}"
fi
echo "Active in new $runtimes sessions (the example above is what you'll see)."
