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

  # Refuse a bundle that predates the code. "Deploy" always means "ship what's in
  # the tree now" — nobody deploys to reinstall what's already installed. The
  # failure this exists to stop: a leftover .app from an earlier build is valid,
  # correctly versioned and correctly signed, so every sanity check an agent
  # thinks to run passes; it gets swapped in, the swap genuinely succeeds, and
  # the deploy looks clean while silently reinstalling old code. Nothing
  # downstream can detect that — it has to be caught here, against the source.
  BIN="$SRC/Contents/MacOS/$PROC_NAME"
  BUILD_TS=$(stat -f %m "$BIN")
  # Only things that actually end up in the binary. Scoping this matters: judged
  # against *every* commit, a README or script-only change would demand a 10-min
  # rebuild that cannot alter the output — and a guard that cries wolf just
  # teaches everyone to pass ALLOW_STALE=1 by reflex, which is no guard at all.
  BUILD_INPUTS=(src src-tauri package.json package-lock.json)
  if [[ "${ALLOW_STALE:-0}" != "1" ]] && git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1; then
    HEAD_TS=$(git -C "$REPO" log -1 --format=%ct -- "${BUILD_INPUTS[@]}")
    if [[ -n "$HEAD_TS" ]] && (( BUILD_TS < HEAD_TS )); then
      {
        echo "ERROR: this build is older than the code — rebuild before deploying."
        echo "  build: $(date -r "$BUILD_TS" '+%Y-%m-%d %H:%M:%S')  $SRC"
        echo "  newest code: $(date -r "$HEAD_TS" '+%Y-%m-%d %H:%M:%S')  $(git -C "$REPO" log -1 --format='%h %s' -- "${BUILD_INPUTS[@]}")"
        echo "  commits the build is missing:"
        # -n, not `| head` — under `set -e -o pipefail` head closing the pipe
        # SIGPIPEs git and aborts the script with 141 before we reach `exit 1`.
        git -C "$REPO" log -n 10 --format='    %h %s' --since="@$BUILD_TS" -- "${BUILD_INPUTS[@]}"
        echo "  fix:  npm run tauri build   (deliberate stale deploy: ALLOW_STALE=1)"
      } >&2
      exit 1
    fi
    # Uncommitted edits count too — committed-ness isn't what makes code deployed.
    # Ask git which sources actually DIFFER rather than testing mtime alone: a
    # checkout, a rebase or a stray `touch` refreshes mtimes without changing
    # content, and a bare `find -newer` would then demand a pointless rebuild.
    # Content says whether it's unbuilt; mtime only says whether the build saw it.
    STALE_SRC=""
    while IFS= read -r f; do
      [[ -n "$f" && -f "$REPO/$f" ]] || continue
      if (( $(stat -f %m "$REPO/$f") > BUILD_TS )); then
        STALE_SRC="${STALE_SRC}    ${f}"$'\n'
      fi
    done < <(git -C "$REPO" status --porcelain -- "${BUILD_INPUTS[@]}" | cut -c4-)
    if [[ -n "$STALE_SRC" ]]; then
      {
        echo "ERROR: edited sources are newer than this build — rebuild before deploying."
        printf '%s' "$STALE_SRC"
        echo "  fix:  npm run tauri build   (deliberate stale deploy: ALLOW_STALE=1)"
      } >&2
      exit 1
    fi
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

# Stage and vet the new copy BEFORE touching the running app. Everything that can
# fail on its own — the copy, a truncated or unsigned bundle — fails here, while
# maiTerm is still up and the user has lost nothing. Quitting first and discovering
# the problem afterwards leaves them with no running app and nothing installed to
# show for it. It also shrinks the downtime window to a rename plus a launch.
log "staging copy (ditto preserves bundle symlinks/xattrs/perms)…"
STAGE="$DEST.new.$$"
rm -rf "$STAGE"
if ! ditto "$SRC" "$STAGE"; then
  log "ERROR: ditto failed — aborting, nothing swapped, $APP_NAME left running"
  rm -rf "$STAGE"
  exit 1
fi
if [[ ! -x "$STAGE/Contents/MacOS/$PROC_NAME" ]]; then
  log "ERROR: staged copy has no executable — aborting, $APP_NAME left running"
  rm -rf "$STAGE"
  exit 1
fi
if ! codesign --verify --strict "$STAGE" >/dev/null 2>&1; then
  log "ERROR: staged copy fails signature check — aborting, $APP_NAME left running"
  rm -rf "$STAGE"
  exit 1
fi

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

# The process leaving the process table is not the same as LaunchServices having
# let go of it. Replace the bundle in that window and `open` tries to activate a
# registration whose process is already gone — "_LSOpenURLsWithCompletionHandler()
# failed with error -600" (procNotFound), which used to trigger a full rollback of
# a perfectly good build. Earlier deploys only survived by luck: a cold-cache
# `ditto` took ~1s and supplied this delay by accident.
sleep 3

log "swapping in (keep a .bak until launch succeeds)…"
BACKUP="$DEST.bak"
rm -rf "$BACKUP"
if [[ -d "$DEST" ]]; then
  mv "$DEST" "$BACKUP"
fi
mv "$STAGE" "$DEST"

log "clearing quarantine (local build; belt-and-suspenders)…"
xattr -dr com.apple.quarantine "$DEST" >/dev/null 2>&1 || true

# Point LaunchServices at the bundle that is actually there now. We just swapped a
# different bundle onto a path it still has cached from the old one.
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[[ -x "$LSREGISTER" ]] && "$LSREGISTER" -f "$DEST" >/dev/null 2>&1 || true

# Retry before rolling back. A launch failure here is usually transient (see the
# -600 note above), and rolling back on the first one throws away a good build and
# reinstalls stale code — the exact outcome this whole script exists to produce
# reliably. Only a build that refuses to start across every attempt is a real
# failure worth reverting for.
log "launching…"
LAUNCHED=0
for attempt in 1 2 3 4 5; do
  if open "$DEST"; then LAUNCHED=1; break; fi
  log "  launch attempt $attempt failed — retrying in 3s"
  sleep 3
done

if (( LAUNCHED )); then
  rm -rf "$BACKUP"
  log "=== done — auto-resume should rehydrate the agent tab ==="
else
  log "ERROR: launch failed after 5 attempts — restoring backup"
  rm -rf "$DEST"
  [[ -d "$BACKUP" ]] && mv "$BACKUP" "$DEST"
  open "$DEST" >/dev/null 2>&1 || true
  log "=== restored previous build ==="
fi
