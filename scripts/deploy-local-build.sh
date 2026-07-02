#!/usr/bin/env bash
#
# deploy-local-build.sh — swap a freshly-built maiTerm.app over the installed copy and restart it.
#
# WHY THIS EXISTS: an agent (Claude/Codex/…) running inside a maiTerm tab cannot quit→copy→launch
# maiTerm itself — quitting the app kills the agent's PTY mid-command, cancelling the very script
# doing the swap. This script SELF-DETACHES (re-execs under `nohup`, disowned, backgrounded; macOS
# has no `setsid`) so the swap runs OUTSIDE the caller's process tree and survives maiTerm's exit.
# maiTerm's auto-resume then brings the agent session back on relaunch, and it can read the log
# below to confirm the outcome.
#
# USAGE (returns immediately after detaching):
#   scripts/deploy-local-build.sh [SRC_APP]
#     SRC_APP  path to the newly-built .app
#              (default: src-tauri/target/release/bundle/macos/maiTerm.app)
#
# Build and verify FIRST (`npm run tauri:build`), THEN run this.

set -euo pipefail

APP_NAME="maiTerm"
PROC_NAME="aiterm"                     # Contents/MacOS binary name (for pgrep/killall)
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Re-exec sentinel — MUST be an argv flag, NOT an env var. `open` (below) hands our
# environment to the relaunched maiTerm, which hands it to every PTY/shell it spawns.
# An env-var sentinel therefore leaks into the NEXT agent's shell, so the next deploy
# sees the flag already set, SKIPS self-detach, and runs the swap ATTACHED to the
# agent's PTY — SIGKILLed mid-swap (exit 137) the instant maiTerm quits, leaving the
# installed app un-swapped with no relaunch. argv does not leak through open→app→PTY.
DETACH_FLAG="--detached-child"
if [[ "${1:-}" == "$DETACH_FLAG" ]]; then
  DETACHED=1; shift
else
  DETACHED=0
fi

SRC="${1:-$REPO/src-tauri/target/release/bundle/macos/$APP_NAME.app}"
DEST="/Applications/$APP_NAME.app"
LOG="$HOME/Library/Logs/com.aiterm.app/maiterm-deploy.log"
mkdir -p "$(dirname "$LOG")"

# ---- self-detach: the swap must outlive the maiTerm PTY that launched it ----
if [[ "$DETACHED" != "1" ]]; then
  # Validate synchronously so the caller sees a bad/missing build immediately.
  if [[ ! -x "$SRC/Contents/MacOS/$PROC_NAME" ]]; then
    echo "ERROR: no valid maiTerm build at: $SRC" >&2
    exit 1
  fi
  nohup "$0" "$DETACH_FLAG" "$SRC" >>"$LOG" 2>&1 </dev/null &
  disown || true
  echo "Detached deploy started (pid $!)."
  echo "Log: $LOG"
  exit 0
fi

# ---- detached body ----
log() { echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"; }
log "=== maiTerm deploy: $SRC -> $DEST ==="

# Don't leak THIS agent's Claude Code session identity into the relaunched app.
# `open` hands our env to the new maiTerm, which would forward CLAUDE_CODE_CHILD_SESSION
# into the auto-resumed claude → it comes up as a "child session" and silently stops
# writing its transcript to disk (chat-history loss). maiTerm also scrubs these at PTY
# spawn (AGENT_ENV_MARKERS) as the real fix; keep the app process itself clean too.
unset CLAUDE_CODE_CHILD_SESSION CLAUDE_CODE_SESSION_ID CLAUDE_CODE_ENTRYPOINT CLAUDE_CODE_EXECPATH CLAUDECODE
# Belt-and-suspenders: scrub the legacy env-var sentinel too, so a shell inside an app
# launched by a PRE-argv build of this script stops propagating MAITERM_DEPLOY_DETACHED=1.
unset MAITERM_DEPLOY_DETACHED

# Give the caller time to FINISH before we pull the rug: not just the tool-call
# returning, but the agent's whole turn — final assistant message flushed to the
# session .jsonl, Stop hooks run. 3s proved tight for a session mid-activity.
sleep 15

log "quitting $APP_NAME (graceful → state saves, auto-resume works)…"
osascript -e "tell application \"$APP_NAME\" to quit" >/dev/null 2>&1 || true

# Wait up to 30s for a clean exit.
for _ in $(seq 1 60); do
  if ! pgrep -x "$PROC_NAME" >/dev/null 2>&1; then break; fi
  sleep 0.5
done
if pgrep -x "$PROC_NAME" >/dev/null 2>&1; then
  log "still running after 30s — forcing quit"
  killall "$PROC_NAME" >/dev/null 2>&1 || true
  sleep 2
fi

log "staging copy (ditto preserves bundle symlinks/xattrs/perms)…"
STAGE="$DEST.new.$$"
rm -rf "$STAGE"
if ! ditto "$SRC" "$STAGE"; then
  log "ERROR: ditto failed — aborting, installed app untouched"
  exit 1
fi

log "swapping in (keep a .bak until launch succeeds)…"
BACKUP="$DEST.bak"
rm -rf "$BACKUP"
if [[ -d "$DEST" ]]; then
  mv "$DEST" "$BACKUP"
fi
mv "$STAGE" "$DEST"

log "clearing quarantine (local build; belt-and-suspenders)…"
xattr -dr com.apple.quarantine "$DEST" >/dev/null 2>&1 || true

log "launching…"
if open "$DEST"; then
  rm -rf "$BACKUP"
  log "=== done — auto-resume should rehydrate the agent tab ==="
else
  log "ERROR: launch failed — restoring backup"
  rm -rf "$DEST"
  [[ -d "$BACKUP" ]] && mv "$BACKUP" "$DEST"
  open "$DEST" >/dev/null 2>&1 || true
  log "=== restored previous build ==="
fi
