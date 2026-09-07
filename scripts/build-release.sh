#!/usr/bin/env bash
#
# build-release.sh — `tauri build`, with the DMG step's Finder race handled.
#
# WHY THIS EXISTS: a DMG's window layout (icon positions, window size, background) lives in
# the volume's .DS_Store. That file's format is undocumented, and Finder is the only writer
# Apple supports — so Tauri's vendored create-dmg (bundle_dmg.sh) mounts the disk image and
# drives Finder over AppleScript to lay it out. Before doing that it waits a flat 2 seconds
# for Finder to register the freshly attached volume. Finder takes 1-3. So every few builds
# lose that race and die with:
#
#   failed to bundle project error running bundle_dmg.sh
#
# ...at the very last step, after the .app is fully built and Developer ID signed. (The
# underlying AppleScript error, "Can't get disk (-1728)", is swallowed unless you pass
# --verbose.) Seen 2026-08-29, 09-01 and 09-06.
#
# WHY NOT JUST FIX THE SLEEP: because the patch has nowhere to live. The Tauri CLI rewrites
# bundle_dmg.sh into src-tauri/target/release/bundle/dmg/ on every single build, immediately
# before executing it, and offers no hook in between and no config for supplying your own.
# Verified 2026-09-07: a copy patched to poll Finder instead of sleeping was silently
# overwritten by the next build, which then ran the stock 2-second sleep. target/ is also
# gitignored. Replacing the sleep with a poll is the right fix and belongs upstream in
# tauri-bundler, not here.
#
# So: retry. The failure is transient and a retry is cheap — Rust is already compiled, so a
# second pass costs about a minute. Retrying the WHOLE build rather than just the DMG is
# deliberate: the updater .tar.gz and its signature are produced after the DMG, so a failed
# run loses those too, and only a full re-run puts every artifact back in sync.
#
# USAGE:
#   scripts/build-release.sh [extra args passed through to `tauri build`]
#
# Signing and notarization environment is the caller's business — see the build-release
# playbook. For a local deploy you don't need the DMG at all: the .app is signed before
# bundling, so scripts/deploy-local-build.sh works even from a run that failed here.

# NOTE: no `set -e`. This script's whole job is to inspect a failing build and decide
# whether to run it again, which means it must survive the failure.
set -uo pipefail

MAX_ATTEMPTS=3
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DMG_DIR="$ROOT_DIR/src-tauri/target/release/bundle/dmg"

cd "$ROOT_DIR"

# A run that dies inside bundle_dmg.sh can leave the interstitial image still attached and a
# rw.*.dmg scratch file behind. Both make the NEXT attempt fail for an unrelated reason (a
# stale mount is the usual explanation for a -1728 that repeats), so clear them first.
clean_dmg_leftovers() {
  local dev
  while read -r dev; do
    [[ -n "$dev" ]] || continue
    echo "Detaching leftover disk image $dev"
    hdiutil detach "$dev" -force >/dev/null 2>&1 || true
  done < <(hdiutil info | awk '/^\/dev\/disk/ && /maiTerm/ { print $1 }')
  rm -f "$DMG_DIR"/rw.*.dmg
}

attempt=1
while (( attempt <= MAX_ATTEMPTS )); do
  log="$(mktemp -t maiterm-build)"

  if (( attempt > 1 )); then
    echo "=== retrying build (attempt $attempt/$MAX_ATTEMPTS) ==="
  fi

  # `${@+"$@"}` not `"$@"`: macOS ships bash 3.2, where an empty "$@" under `set -u` is an
  # unbound variable.
  npm run tauri build -- ${@+"$@"} 2>&1 | tee "$log"
  status=${PIPESTATUS[0]}

  if (( status == 0 )); then
    rm -f "$log"
    (( attempt > 1 )) && echo "=== build OK on attempt $attempt ==="
    exit 0
  fi

  if ! grep -q 'error running bundle_dmg.sh' "$log"; then
    rm -f "$log"
    echo >&2 "=== build failed (exit $status) — not the DMG race, so not retrying ==="
    exit "$status"
  fi

  rm -f "$log"
  echo >&2 "=== DMG step lost the Finder race (attempt $attempt) ==="
  clean_dmg_leftovers
  (( attempt++ ))
done

echo >&2 "=== the DMG step failed $MAX_ATTEMPTS times, which is no longer a flake ==="
echo >&2 "Look for a stale /Volumes/maiTerm* mount, or re-run with --verbose to see the"
echo >&2 "AppleScript error that Tauri swallows. The .app itself is built and signed either"
echo >&2 "way, so scripts/deploy-local-build.sh still works from it."
exit 1
