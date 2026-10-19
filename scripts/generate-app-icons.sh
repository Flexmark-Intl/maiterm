#!/usr/bin/env bash
#
# Generates maiTerm's brand assets: the Poppins wordmark, the macOS 26
# "Liquid Glass" app icon (Icon Composer .icon), and the website logo/favicon.
#
# On macOS 26+ (Xcode 26 / actool >= 26), Tauri 2.11+ compiles the .icon listed
# in tauri.conf `bundle > icon` into an Assets.car and wires CFBundleIconName, so
# the dock/Finder icon adapts across Default / Dark / Tinted / Clear appearances
# automatically. Older macOS, Windows and Linux fall back to the classic flat
# flat icon (icon.icns / icon.png), which Tauri still generates from the PNGs.
#
# Design: the maiSoft house icon (2026-10-19) — "mai" over "term" in Poppins, stone
# (#a8a29e) on a dark stone ground. It replaced the dots emblem (scripts/generate-emblem.py;
# to roll back, check out this script, that one and src-tauri/icons from before the switch).
#
# Requires: ImageMagick, python3 with Pillow + numpy, Xcode 26 (actool + `ictool`).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICONS="$ROOT/src-tauri/icons"
STATIC="$ROOT/static"
WEB="$ROOT/website"
ICON="$ICONS/AppIcon.icon"
ICTOOL="/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool"
BRAND="$ROOT/scripts/make-brand.py"          # draws every piece; fonts vendored beside it

# --- the art: scripts/make-brand.py draws every piece in the maiSoft house style ---
# ("mai" in Poppins Bold in the product colour over "term" in Poppins SemiBold, on a
# ground tinted toward it — maiProjects/brand/make-icon.py is the family's reference).
# This script only turns those pieces into the files that ship.
command -v magick >/dev/null 2>&1 || { echo "ERROR: ImageMagick (magick) not found" >&2; exit 1; }
ART="$(mktemp -d)"
python3 "$BRAND" "$ART"

# --- wordmark: static/logo-light.png (dark themes) + logo-dark.png (light themes) ---
# Horizontal "mai" + "term". Its ratio feeds the hardcoded `aspect-ratio` in
# WorkspaceSidebar/.sidebar-logo, +page/.loading-logo and SplitPane/.empty-logo —
# update all three if the lettering or size changes.
magick "$ART/logo-light.png" -strip "$STATIC/logo-light.png"
magick "$ART/logo-dark.png" -strip "$STATIC/logo-dark.png"
echo "wrote $STATIC/logo-light.png + logo-dark.png ($(magick identify -format '%wx%h' "$STATIC/logo-light.png"))"

# --- author the .icon (icon.json + Assets/mai.png + Assets/word.png) ---
# Two glass layers carrying their own colour ("mai" in the stone gradient, "term" in
# white) over the dark stone ground. Icon Composer adds specular, blur and shadow from
# each layer's alpha, so nothing here may carry baked lighting; the tinted and clear
# appearances drop the colour and keep the lettering, which still reads as the name.
rm -rf "$ICON"; mkdir -p "$ICON/Assets"
magick "$ART/glass-mai.png" -strip "$ICON/Assets/mai.png"
magick "$ART/glass-word.png" -strip "$ICON/Assets/word.png"
BG_FILL="$(cat "$ART/glass-ground.txt")"
cat > "$ICON/icon.json" <<JSON
{
  "fill": { "automatic-gradient": "$BG_FILL" },
  "groups": [
    {
      "layers": [
        { "blend-mode": "normal", "glass": true, "hidden": false,
          "image-name": "word.png", "name": "word",
          "position": { "scale": 1, "translation-in-points": [0, 0] } },
        { "blend-mode": "normal", "glass": true, "hidden": false,
          "image-name": "mai.png", "name": "mai",
          "position": { "scale": 1, "translation-in-points": [0, 0] } }
      ],
      "shadow": { "kind": "neutral", "opacity": 0.5 },
      "translucency": { "enabled": false, "value": 0.5 }
    }
  ],
  "supported-platforms": { "circles": ["watchOS"], "squares": "shared" }
}
JSON
echo "wrote $ICON (icon.json + Assets/mai.png + word.png)"

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

# --- FLAT tiles: the non-Liquid-Glass icon, for Windows, Linux, pre-macOS-26 ---
# Also the website header, which Starlight swaps by theme (the light tile on light
# pages, the dark one on dark). The full-bleed art is masked into the rounded tile
# with its margin and drop shadow, because nothing masks it for us on these platforms.
NAVY="$ICONS/icon-source-1024.png"   # the name predates the stone ground; it is the dark master
FW="$(mktemp -d)"
# tile silhouette (the slight downward offset is the original's, kept for continuity)
magick -size 1024x1024 xc:black -fill white -draw 'roundrectangle 88,72 935,919 165,165' "$FW/mask.png"
# Soft ground shadow, from the same silhouette. The -colorspace sRGB here and in
# the composite below are load-bearing: a drawn mask is a GRAYSCALE image, and the
# first image in a composite sets the output colorspace — without these the whole
# tile is written greyscale and every colour collapses to its red channel.
# NO -negate here. The mask is white INSIDE the silhouette, which is already the
# alpha a drop shadow wants; negating it paints opaque black everywhere OUTSIDE
# the tile instead, giving a black square with the tile inset in it. That shipped
# once — corner alpha 0 -> 1 on every tile and both favicons.
magick "$FW/mask.png" -blur 0x26 -roll +0+16 "$FW/sm.png"
magick -size 1024x1024 xc:'#000' -colorspace sRGB "$FW/sm.png" -alpha off \
  -compose CopyOpacity -composite -channel A -evaluate multiply 0.42 +channel \
  -colorspace sRGB -type TrueColorAlpha "$FW/shadow.png"

flat_tile() {  # $1=full-bleed art  $2=output
  # The art fills the silhouette's box (848 square at 88,72), then takes its corners.
  magick -size 1024x1024 xc:none -colorspace sRGB \
    \( "$1" -resize 848x848 \) -geometry +88+72 -compose over -composite "$FW/a.png"
  magick "$FW/a.png" -colorspace sRGB "$FW/mask.png" -alpha off \
    -compose CopyOpacity -composite -colorspace sRGB -type TrueColorAlpha "$FW/t.png"
  # -strip on every output: without it ImageMagick embeds a png:tIME chunk and
  # the file churns on every run even when the pixels are identical.
  magick "$FW/shadow.png" -colorspace sRGB -type TrueColorAlpha \
    "$FW/t.png" -compose over -composite -type TrueColorAlpha -strip "$2"
}

flat_tile "$ART/art-dark.png" "$NAVY"
flat_tile "$ART/art-light.png" "$WEB/src/assets/icon-light.png"
cp "$NAVY" "$WEB/src/assets/icon-dark.png"
magick "$NAVY" -resize 256x256 -strip "$WEB/public/favicon.png"
magick "$NAVY" -resize 256x256 -strip "$STATIC/favicon.png"

# The files tauri.conf.json `bundle.icon` actually ships. Regenerating only the
# 1024 master leaves these stale: the bundler synthesises the macOS .icns and the
# Windows .ico from THIS list, not from the master, so a rebrand that skips them
# ships the old mark everywhere except macOS 26's Assets.car.
# icon.png is 512, NOT 1024. The bundler maps each of these to an icns type by
# (pixel size, density), and density is 1 unless the NAME says @2x — so 512 is the
# largest a plain name can be. At 1024 it matches nothing and the macOS bundle dies
# on "No matching IconType", taking the whole build with it.
for spec in "32x32.png 32" "64x64.png 64" "128x128.png 128" "128x128@2x.png 256" "icon.png 512"; do
  set -- $spec
  magick "$NAVY" -resize "$2x$2" -strip "$ICONS/$1"
done
magick "$NAVY" -define icon:auto-resize=256,128,64,48,32,16 -strip "$ICONS/icon.ico"
rm -rf "$FW" "$ART"
echo "wrote $NAVY + bundle PNGs + icon.ico + website icon-light/icon-dark/favicon + static/favicon"

# --- optional Liquid Glass appearance preview sheet for iteration ---
if [ "${PREVIEW:-0}" = "1" ] && [ -x "$ICTOOL" ]; then
  for r in Default Dark TintedLight TintedDark ClearDark Mono; do
    "$ICTOOL" "$ICON" --export-image --output-file "/tmp/lg_$r.png" \
      --platform macOS --rendition "$r" --width 512 --height 512 --scale 1 2>/dev/null || true
  done
  echo "previews: /tmp/lg_<rendition>.png"
fi
