#!/bin/bash
# Cargo runner for macOS (src-tauri/.cargo/config.toml): wrap the dev binary in a minimal
# .app bundle, sign it with a stable identity, then run it from inside the bundle.
#
# Why a bundle: `cargo run` starts a bare binary, and a bare binary has no bundle identifier.
# macOS keys per-app settings on that identifier — most visibly the Dock's Options > Assign To
# (This Desktop / All Desktops), which Spaces stores in com.apple.spaces `app-bindings`. A bare
# binary lands in the one shared ":no-bundle:????" slot, which window placement ignores, so the
# dev app opened on whatever desktop was in front. Run from Contents/MacOS of a bundle whose
# Info.plist says com.aiterm.dev, it is an app of its own: assignment sticks, and the Dock shows
# it as "maiTerm Dev". The executable keeps the name `aiterm` (another_instance_running in
# state/persistence.rs matches on the file name).
#
# Why sign: a debug build is ad-hoc signed by the linker, and an ad-hoc signature's designated
# requirement is the binary's cdhash, which changes on every rebuild. The keychain ACL on our
# vault items (accounts/vault.rs) is keyed on that requirement, so every rebuild re-prompted once
# per stored token. Signed with a certificate, the requirement becomes "this identifier, this
# certificate", which survives rebuilds: "Always Allow" sticks.
#
# Identity: $MAITERM_DEV_SIGN_IDENTITY (a name or SHA-1), else the first "Apple Development"
# identity in the keychain. With none, the bundle stays ad-hoc and runs as before.
set -u

bin="$1"
shift

if [[ "$(basename "$bin")" != "aiterm" ]]; then
  exec "$bin" "$@"
fi

app="$(dirname "$bin")/maiTerm Dev.app"
mkdir -p "$app/Contents/MacOS"
# A clone, not a hard link: codesign rewrites the file, and a fresh copy per run follows
# cargo replacing the binary on every rebuild. APFS clones are instant; fall back to a copy.
rm -f "$app/Contents/MacOS/aiterm"
cp -c "$bin" "$app/Contents/MacOS/aiterm" 2>/dev/null || cp "$bin" "$app/Contents/MacOS/aiterm"

cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.aiterm.dev</string>
	<key>CFBundleName</key>
	<string>maiTerm Dev</string>
	<key>CFBundleExecutable</key>
	<string>aiterm</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSDesktopFolderUsageDescription</key>
	<string>maiTerm needs access to your Desktop folder so terminal commands and processes can read and write files there.</string>
	<key>NSDocumentsFolderUsageDescription</key>
	<string>maiTerm needs access to your Documents folder so terminal commands and processes can read and write files there.</string>
	<key>NSDownloadsFolderUsageDescription</key>
	<string>maiTerm needs access to your Downloads folder so terminal commands and processes can read and write files there.</string>
</dict>
</plist>
PLIST

# WebKit keeps a page's localStorage under the process's bundle id; the bare binary used its
# file name. Carry the old store over once so the dev app's remembered UI state survives.
if [[ -d "$HOME/Library/WebKit/aiterm" && ! -e "$HOME/Library/WebKit/com.aiterm.dev" ]]; then
  cp -R "$HOME/Library/WebKit/aiterm" "$HOME/Library/WebKit/com.aiterm.dev" 2>/dev/null || true
fi

identity="${MAITERM_DEV_SIGN_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  identity=$(security find-identity -v -p codesigning 2>/dev/null \
    | awk '/"Apple Development:/ { print $2; exit }')
fi
if [[ -n "$identity" ]]; then
  codesign --force --sign "$identity" --identifier com.aiterm.dev "$app" >/dev/null 2>&1 \
    || echo "dev-sign-run: codesign failed, running ad-hoc (keychain will prompt)" >&2
else
  # The linker's ad-hoc signature covers the binary but not the bundle's Info.plist, and a
  # bundle whose seal doesn't match can be refused; re-seal it ad-hoc.
  codesign --force --sign - --identifier com.aiterm.dev "$app" >/dev/null 2>&1 || true
fi

exec "$app/Contents/MacOS/aiterm" "$@"
