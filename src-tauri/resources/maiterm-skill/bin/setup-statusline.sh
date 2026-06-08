#!/usr/bin/env bash
# maiTerm Claude Code status line — local installer.
#
# Optimized for the happy path: when jq is already present this does the whole
# job in one shot (render example → copy script → merge settings.json) with no
# prompts. It NEVER installs jq itself — if jq is missing it prints the exact
# command for the detected package manager and exits 3 so the caller (the
# /maiterm statusline skill) can offer to run it interactively.
#
# Exit codes:
#   0  installed
#   3  jq missing — a line "JQ_MISSING:<install command>" was printed
#   1  unexpected error
set -e

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAYLOAD="$SKILL_DIR/statusline-command.sh"
CLAUDE_DIR="$HOME/.claude"
DEST="$CLAUDE_DIR/statusline-command.sh"
SETTINGS="$CLAUDE_DIR/settings.json"

[ -f "$PAYLOAD" ] || { echo "error: bundled statusline-command.sh not found at $PAYLOAD" >&2; exit 1; }

# --- Render a representative example using REAL colors + this machine's values,
#     so the user sees exactly what their status line will look like. ---
GREEN='\033[1;32m'; CYAN='\033[1;36m'; BLUE='\033[1;34m'
MAGENTA='\033[1;35m'; YELLOW='\033[1;33m'; ORANGE='\033[38;5;208m'; RESET='\033[0m'
host=$(hostname -s 2>/dev/null | tr '[:lower:]' '[:upper:]'); [ -z "$host" ] && host="HOST"

echo "maiTerm Claude Code status line — example:"
echo
printf '  %b%s %s%b[%s]%b %b(%s)%b %b%s%b %b%s%b %b%s%%%b\n' \
  "$GREEN" "$host" "$(whoami)" \
  "$CYAN" "~/projects/maiterm" "$RESET" \
  "$ORANGE" "main" "$RESET" \
  "$BLUE" "opus-4-8[1m]" "$RESET" \
  "$YELLOW" "high" "$RESET" \
  "$MAGENTA" "12.3" "$RESET"
echo
echo "  host+user · [cwd] · (git branch) · model · effort · context-used%"
echo

# --- jq gate: required to parse Claude's JSON at runtime AND to merge settings.
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

# --- Install: copy the bundled script + merge the statusLine key into
#     settings.json, preserving every other key. Idempotent. ---
mkdir -p "$CLAUDE_DIR"
install -m 0755 "$PAYLOAD" "$DEST"

[ -f "$SETTINGS" ] || echo '{}' > "$SETTINGS"
tmp=$(mktemp)
jq --arg cmd "bash $DEST" \
   '.statusLine = {type:"command", command:$cmd}' \
   "$SETTINGS" > "$tmp" && mv "$tmp" "$SETTINGS"

echo "Installed → $DEST"
echo "Active in new Claude Code sessions (the example above is what you'll see)."
