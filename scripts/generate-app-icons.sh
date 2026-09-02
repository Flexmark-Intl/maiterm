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

EMBLEM="$ROOT/scripts/generate-emblem.py"    # the mark, drawn parametrically
# Periwinkle #7880BE in extended-srgb (system derives the gradient + appearances)
BG_FILL='extended-srgb:0.47059,0.50196,0.74510,1.00000'
MARK_W=600                                   # mark width on the 1024 canvas (~58%)

# --- wordmark: static/logo-light.png (dark themes) + logo-dark.png (light themes) ---
# IBM Plex Mono Regular, duo-tone. "mai" recedes, "Term" carries the accent.
# The in-app "mai" is deliberately LIGHTER than the marketing value (#5C6173):
# the loading and empty-pane logos knock it back with CSS opacity (.5 and .3) and
# a slate that dark dissolves at .3.
# The two tones need LUMINANCE separation, not just a hue difference. #87A0E6 sat
# at nearly the same brightness as the "mai" slate (1.20:1) and read as one colour
# at 13px; #A8BCFF lifts that to 1.65:1. The light-theme pair is unaffected — its
# Term is a much deeper #5965D6, which needs to stay dark against cream.
# Ratio feeds the hardcoded `aspect-ratio` in WorkspaceSidebar/.sidebar-logo,
# +page/.loading-logo and SplitPane/.empty-logo — update all three if the glyph
# set or size changes.
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
  wordmark '#8B93A7' '#A8BCFF' "$STATIC/logo-light.png"
  wordmark '#5C6173' '#5965D6' "$STATIC/logo-dark.png"
  echo "wrote $STATIC/logo-light.png + logo-dark.png ($(magick identify -format '%wx%h' "$STATIC/logo-light.png"))"
else
  echo "WARN: magick or vendored Plex Mono missing; skipped wordmark" >&2
fi

# --- author the .icon (icon.json + Assets/mark.png) ---
# The mark is drawn white-on-transparent: Icon Composer shapes and lights the
# GLASS LAYER from its alpha, so the alpha has to be the true silhouette. It adds
# its own specular, blur and shadow -- nothing here may carry baked lighting.
# Everything this needs is checked BEFORE the rm -rf: AppIcon.icon/{icon.json,
# Assets/mark.png} are tracked files, and wiping them before knowing we can
# rebuild them turns a missing dependency into a deleted build input.
if ! command -v rsvg-convert >/dev/null 2>&1; then
  echo "ERROR: rsvg-convert not found (brew install librsvg) — cannot render the emblem" >&2; exit 1
fi
if ! python3 "$EMBLEM" --fg '#ffffff' --transparent > /dev/null 2>&1; then
  echo "ERROR: $EMBLEM failed under $(python3 -V 2>&1) — cannot render the emblem" >&2; exit 1
fi
EMW="$(mktemp -d)"
rm -rf "$ICON"; mkdir -p "$ICON/Assets"
python3 "$EMBLEM" --fg '#ffffff' --transparent > "$EMW/emblem.svg"
rsvg-convert -w 1024 -h 1024 "$EMW/emblem.svg" -o "$EMW/emblem.png"
magick "$EMW/emblem.png" -trim +repage -resize ${MARK_W}x \
  -background none -gravity center -extent 1024x1024 -strip "$ICON/Assets/mark.png"
rm -rf "$EMW"
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

# --- FLAT tiles: the non-Liquid-Glass icon, for Windows, Linux, pre-macOS-26 ---
# Also the website header, which Starlight swaps by theme (cream on light pages,
# navy on dark). Flat tiles read better than the glass render on a flat header.
#
# The navy master used to be a committed, hand-made PNG; it is generated now, so
# the emblem has exactly one source. Both tiles stay MONOCHROME on their ground,
# matching what the old "m" did — the active/idle distinction is carried by node
# size, so no accent colour is needed here any more than in the glass layer.
NAVY="$ICONS/icon-source-1024.png"
if command -v magick >/dev/null 2>&1 && command -v rsvg-convert >/dev/null 2>&1; then
  FW="$(mktemp -d)"
  # tile silhouette (the slight downward offset is the original's, kept for continuity)
  magick -size 1024x1024 xc:black -fill white -draw 'roundrectangle 88,72 935,919 165,165' "$FW/mask.png"
  # Soft ground shadow, from the same silhouette. The -colorspace sRGB here and in
  # the composite below are load-bearing: a drawn mask is a GRAYSCALE image, and the
  # first image in a composite sets the output colorspace — without these the whole
  # tile is written greyscale and every colour collapses to its red channel
  # (#2A2F4D -> #282828, #5965D6 -> #595959).
  # NO -negate here. The mask is white INSIDE the silhouette, which is already the
  # alpha a drop shadow wants; negating it paints opaque black everywhere OUTSIDE
  # the tile instead, giving a black square with the tile inset in it. That shipped
  # once — corner alpha 0 -> 1 on every tile and both favicons.
  magick "$FW/mask.png" -blur 0x26 -roll +0+16 "$FW/sm.png"
  magick -size 1024x1024 xc:'#000' -colorspace sRGB "$FW/sm.png" -alpha off \
    -compose CopyOpacity -composite -channel A -evaluate multiply 0.42 +channel \
    -colorspace sRGB -type TrueColorAlpha "$FW/shadow.png"

  flat_tile() {  # $1=gradient stops  $2=mark colour  $3=output
    python3 "$EMBLEM" --fg "$2" --transparent > "$FW/e.svg"
    rsvg-convert -w 1024 -h 1024 "$FW/e.svg" -o "$FW/e.png"
    magick "$FW/e.png" -trim +repage -resize 500x "$FW/mk.png"
    magick -size 1024x1024 canvas:none -sparse-color barycentric "$1" "$FW/g.png"
    magick "$FW/g.png" -colorspace sRGB "$FW/mask.png" -alpha off \
      -compose CopyOpacity -composite -colorspace sRGB -type TrueColorAlpha "$FW/t.png"
    magick "$FW/shadow.png" -colorspace sRGB -type TrueColorAlpha \
      "$FW/t.png" -compose over -composite "$FW/ts.png"
    # tile centre, not canvas centre: the silhouette sits 16px above centre
    # (its box is 88,72-935,919, so its middle is y=495.5 against a canvas 512).
    # -strip on every output: without it ImageMagick embeds a png:tIME chunk and
    # the file churns on every run even when the pixels are identical.
    magick "$FW/ts.png" "$FW/mk.png" -gravity center -geometry +0-16 \
      -compose over -composite -type TrueColorAlpha -strip "$3"
  }

  flat_tile '0,0 #2A2F4D 1023,1023 #1E2133' '#E1E3F0' "$NAVY"
  flat_tile '0,0 #FFFDF7 1023,1023 #F2E9D6' '#5965D6' "$WEB/src/assets/icon-light.png"
  cp "$NAVY" "$WEB/src/assets/icon-dark.png"
  magick "$NAVY" -resize 256x256 -strip "$WEB/public/favicon.png"
  magick "$NAVY" -resize 256x256 -strip "$STATIC/favicon.png"

  # The files tauri.conf.json `bundle.icon` actually ships. Regenerating only the
  # 1024 master leaves these stale: the bundler synthesises the macOS .icns and the
  # Windows .ico from THIS list, not from the master, so a rebrand that skips them
  # ships the old mark everywhere except macOS 26's Assets.car.
  for spec in "32x32.png 32" "64x64.png 64" "128x128.png 128" "128x128@2x.png 256" "icon.png 1024"; do
    set -- $spec
    magick "$NAVY" -resize "$2x$2" -strip "$ICONS/$1"
  done
  magick "$NAVY" -define icon:auto-resize=256,128,64,48,32,16 -strip "$ICONS/icon.ico"
  rm -rf "$FW"
  echo "wrote $NAVY + bundle PNGs + icon.ico + website icon-light/icon-dark/favicon + static/favicon"
else
  echo "WARN: magick or rsvg-convert missing; skipped flat tiles" >&2
fi

# --- optional Liquid Glass appearance preview sheet for iteration ---
if [ "${PREVIEW:-0}" = "1" ] && [ -x "$ICTOOL" ]; then
  for r in Default Dark TintedLight TintedDark ClearDark Mono; do
    "$ICTOOL" "$ICON" --export-image --output-file "/tmp/lg_$r.png" \
      --platform macOS --rendition "$r" --width 512 --height 512 --scale 1 2>/dev/null || true
  done
  echo "previews: /tmp/lg_<rendition>.png"
fi
