#!/usr/bin/env bash
#
# Generates maiTerm's macOS 26 "Liquid Glass" app icon (Icon Composer .icon) and
# the website logo/favicon rendered from it.
#
# On macOS 26+ (Xcode 26 / actool >= 26), Tauri 2.11+ compiles the .icon listed
# in tauri.conf `bundle > icon` into an Assets.car and wires CFBundleIconName, so
# the dock/Finder icon adapts across Default / Dark / Tinted / Clear appearances
# automatically. Older macOS, Windows and Linux fall back to the classic flat
# navy icon (icon.icns / icon.png), which Tauri still generates from the PNGs.
#
# Design (approved): periwinkle automatic-gradient glass tile + the white "m"
# mark as a single glass layer (system-managed monochrome, subtle in dark).
#
# Requires: ImageMagick, Xcode 26 (provides actool + Icon Composer's `ictool`).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICONS="$ROOT/src-tauri/icons"
STATIC="$ROOT/static"
WEB="$ROOT/website"
ICON="$ICONS/AppIcon.icon"
ICTOOL="/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool"

MARK="$STATIC/logo-mark-light.png"          # white "m" + periwinkle accents
# Periwinkle #7880BE in extended-srgb (system derives the gradient + appearances)
BG_FILL='extended-srgb:0.47059,0.50196,0.74510,1.00000'
MARK_W=600                                   # mark width on the 1024 canvas (~58%)

# --- author the .icon (icon.json + Assets/mark.png) ---
rm -rf "$ICON"; mkdir -p "$ICON/Assets"
magick "$MARK" -resize ${MARK_W}x -background none -gravity center -extent 1024x1024 "$ICON/Assets/mark.png"
cat > "$ICON/icon.json" <<JSON
{
  "fill": { "automatic-gradient": "$BG_FILL" },
  "groups": [
    {
      "layers": [
        { "blend-mode": "normal", "fill": "automatic", "glass": true, "hidden": false,
          "image-name": "mark.png", "name": "mark",
          "position": { "scale": 1, "translation-in-points": [0, 0] } }
      ],
      "shadow": { "kind": "neutral", "opacity": 0.5 },
      "translucency": { "enabled": true, "value": 0.5 }
    }
  ],
  "supported-platforms": { "circles": ["watchOS"], "squares": "shared" }
}
JSON
echo "wrote $ICON (icon.json + Assets/mark.png)"

# --- compile the .icon into a precompiled Assets.car (committed build input) ---
# We ship the prebuilt Assets.car rather than letting `tauri build` invoke actool,
# because actool's ibtoold daemon wedges after one compile (cryptic "failed to run
# actool" / "insert nil object"), which would randomly break signed release builds.
# Tauri copies a precompiled .car directly and still sets CFBundleIconName from it.
if command -v actool >/dev/null 2>&1; then
  killall ibtoold 2>/dev/null || true   # reset the daemon so the single compile is clean
  CARW="$(mktemp -d)"; cp -R "$ICON" "$CARW/Icon.icon"; mkdir -p "$CARW/out"
  actool "$CARW/Icon.icon" --compile "$CARW/out" --output-format human-readable-text \
    --notices --warnings --output-partial-info-plist "$CARW/out/info.plist" \
    --app-icon Icon --include-all-app-icons --accent-color AccentColor \
    --enable-on-demand-resources NO --development-region en --target-device mac \
    --minimum-deployment-target 26.0 --platform macosx
  if [ -f "$CARW/out/Assets.car" ]; then
    cp "$CARW/out/Assets.car" "$ICONS/Assets.car"
    echo "wrote $ICONS/Assets.car ($(wc -c < "$ICONS/Assets.car") bytes)"
  else
    echo "ERROR: actool did not produce Assets.car (try: killall ibtoold; rerun)" >&2; exit 1
  fi
  rm -rf "$CARW"
else
  echo "WARN: actool not found; kept existing $ICONS/Assets.car" >&2
fi

# --- render website logo + favicon from the .icon (Default = periwinkle glass) ---
if [ -x "$ICTOOL" ]; then
  "$ICTOOL" "$ICON" --export-image --output-file "$WEB/src/assets/icon.png" \
    --platform macOS --rendition Default --width 1024 --height 1024 --scale 1
  "$ICTOOL" "$ICON" --export-image --output-file /tmp/_favicon.png \
    --platform macOS --rendition Default --width 256 --height 256 --scale 1
  magick /tmp/_favicon.png -resize 256x256 "$WEB/public/favicon.png"; rm -f /tmp/_favicon.png
  echo "rendered website/src/assets/icon.png + website/public/favicon.png"
  # optional appearance preview sheet for iteration
  if [ "${PREVIEW:-0}" = "1" ]; then
    for r in Default Dark TintedLight TintedDark ClearDark Mono; do
      "$ICTOOL" "$ICON" --export-image --output-file "/tmp/lg_$r.png" \
        --platform macOS --rendition "$r" --width 512 --height 512 --scale 1 2>/dev/null || true
    done
    echo "previews: /tmp/lg_<rendition>.png"
  fi
else
  echo "WARN: ictool not found ($ICTOOL); skipped website/favicon render" >&2
fi
