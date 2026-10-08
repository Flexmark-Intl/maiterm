#!/bin/bash
# Cargo runner for macOS (src-tauri/.cargo/config.toml): sign the dev binary with a stable
# identity, then run it.
#
# Why: a debug build is ad-hoc signed by the linker, and an ad-hoc signature's designated
# requirement is the binary's cdhash, which changes on every rebuild. The keychain ACL on our
# vault items (accounts/vault.rs) is keyed on that requirement, so every rebuild re-prompted once
# per stored token. Signed with a certificate, the requirement becomes "this identifier, this
# certificate", which survives rebuilds: "Always Allow" sticks.
#
# Identity: $MAITERM_DEV_SIGN_IDENTITY (a name or SHA-1), else the first "Apple Development"
# identity in the keychain. With none, the binary stays ad-hoc and runs as before.
set -u

bin="$1"
shift

if [[ "$(basename "$bin")" == "aiterm" ]]; then
  identity="${MAITERM_DEV_SIGN_IDENTITY:-}"
  if [[ -z "$identity" ]]; then
    identity=$(security find-identity -v -p codesigning 2>/dev/null \
      | awk '/"Apple Development:/ { print $2; exit }')
  fi
  if [[ -n "$identity" ]]; then
    codesign --force --sign "$identity" --identifier com.aiterm.dev "$bin" >/dev/null 2>&1 \
      || echo "dev-sign-run: codesign failed, running ad-hoc (keychain will prompt)" >&2
  fi
fi

exec "$bin" "$@"
