#!/usr/bin/env bash
#
# Generates maiTerm's brand assets: the IBM Plex Mono wordmark, the macOS 26
# "Liquid Glass" app icon (Icon Composer .icon), and the website logo/favicon.
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
PLEX="$ROOT/scripts/assets/fonts/IBMPlexMono-Regular.ttf"   # vendored, OFL-1.1 (see OFL.txt)

MARK="$STATIC/logo-mark-light.png"          # white "m" + periwinkle accents
# Periwinkle #7880BE in extended-srgb (system derives the gradient + appearances)
BG_FILL='extended-srgb:0.47059,0.50196,0.74510,1.00000'
MARK_W=600                                   # mark width on the 1024 canvas (~58%)

# --- wordmark: static/logo-light.png (dark themes) + logo-dark.png (light themes) ---
# IBM Plex Mono Regular, duo-tone. "mai" recedes, "Term" carries the accent.
# The in-app "mai" is deliberately LIGHTER than the marketing value (#5C6173): the
# three consumers knock the logo back with CSS opacity (sidebar .7, loading .5,
# empty pane .3) and a slate that dark dissolves at .3. Ratio feeds the hardcoded
# `aspect-ratio` in WorkspaceSidebar/.sidebar-logo, +page/.loading-logo and
# SplitPane/.empty-logo — update all three if the glyph set or size changes.
if command -v magick >/dev/null 2>&1 && [ -f "$PLEX" ]; then
  wordmark() {  # $1=mai colour  $2=Term colour  $3=output
    local t; t="$(mktemp -d)"
    magick -background none -fill "$1" -font "$PLEX" -pointsize 700 label:'mai'  "$t/a.png"
    magick -background none -fill "$2" -font "$PLEX" -pointsize 700 label:'Term' "$t/b.png"
    # +repage after +append: the appended image keeps the FIRST frame's page
    # geometry, and any later -flatten would crop back to it.
    magick "$t/a.png" "$t/b.png" +append +repage -trim +repage \
      -background none -alpha background -colorspace RGB -resize x320 -colorspace sRGB \
      +repage -strip "$3"
    rm -rf "$t"
  }
  wordmark '#8B93A7' '#87A0E6' "$STATIC/logo-light.png"
  wordmark '#5C6173' '#5965D6' "$STATIC/logo-dark.png"
  echo "wrote $STATIC/logo-light.png + logo-dark.png ($(magick identify -format '%wx%h' "$STATIC/logo-light.png"))"
else
  echo "WARN: magick or vendored Plex Mono missing; skipped wordmark" >&2
fi

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

# --- website logo + favicon: FLAT dark/light icons (not glass) ---
# Starlight swaps these by theme: light theme -> cream tile (black m), dark theme
# -> the navy master. Flat tiles read better than the glass render on flat headers.
NAVY="$ICONS/icon-source-1024.png"
if command -v magick >/dev/null 2>&1 && [ -f "$NAVY" ]; then
  FW="$(mktemp -d)"
  magick -size 1024x1024 xc:black -fill white -draw 'roundrectangle 88,72 935,919 165,165' "$FW/mask.png"
  magick "$NAVY" -alpha extract "$FW/na.png"
  magick -size 1024x1024 xc:black -colorspace sRGB "$FW/na.png" -alpha off \
    -compose CopyOpacity -composite -colorspace sRGB -type TrueColorAlpha "$FW/shadow.png"
  magick "$STATIC/logo-mark-dark.png" -fuzz 22% -fill '#5965D6' -opaque '#7880BE' "$FW/mk.png"
  magick -size 1024x1024 canvas:none -sparse-color barycentric '0,0 #FFFDF7 1023,1023 #F2E9D6' "$FW/g.png"
  magick "$FW/g.png" "$FW/mask.png" -alpha off -compose CopyOpacity -composite "$FW/t.png"
  magick "$FW/shadow.png" -colorspace sRGB -type TrueColorAlpha "$FW/t.png" -compose over -composite "$FW/ts.png"
  magick "$FW/ts.png" \( "$FW/mk.png" -resize 82.6% \) -gravity NorthWest -geometry +242+297 \
    -compose over -composite -type TrueColorAlpha "$WEB/src/assets/icon-light.png"
  cp "$NAVY" "$WEB/src/assets/icon-dark.png"
  magick "$NAVY" -resize 256x256 "$WEB/public/favicon.png"
  rm -rf "$FW"
  echo "wrote website icon-light.png (cream) + icon-dark.png (navy) + favicon.png (navy)"
else
  echo "WARN: magick or navy master missing; skipped website assets" >&2
fi

# --- optional Liquid Glass appearance preview sheet for iteration ---
if [ "${PREVIEW:-0}" = "1" ] && [ -x "$ICTOOL" ]; then
  for r in Default Dark TintedLight TintedDark ClearDark Mono; do
    "$ICTOOL" "$ICON" --export-image --output-file "/tmp/lg_$r.png" \
      --platform macOS --rendition "$r" --width 512 --height 512 --scale 1 2>/dev/null || true
  done
  echo "previews: /tmp/lg_<rendition>.png"
fi
