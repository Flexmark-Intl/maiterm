#!/usr/bin/env bash
# Builds the zip uploaded to the Chrome Web Store: dist/maiterm-chrome-<version>.zip
#
#   scripts/package.sh                 the store package
#   scripts/package.sh --with-key PEM  the FIRST upload only: adds the private key as key.pem,
#                                      which makes the store keep our extension id
#                                      (mdaccealpnngffgibmnijfgakddfgjpa) instead of minting one.
#
# The file list is an allowlist, not an exclude list: a zip built by excluding things is one
# mistake away from shipping the scripts, the store artwork or a stray key to every user.
#
# The manifest's `key` (the PUBLIC key that pins the id for an unpacked load) is stripped: the
# store refuses a manifest carrying one, and signs the package with its own.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

KEY_PEM=""
if [ "${1:-}" = "--with-key" ]; then
  KEY_PEM="${2:?--with-key needs the path to the private key}"
  [ -f "$KEY_PEM" ] || { echo "no such key: $KEY_PEM" >&2; exit 1; }
fi

VERSION="$(node -p "require('./manifest.json').version")"
OUT="dist/maiterm-chrome-${VERSION}.zip"

FILES=(
  background.js
  driver.js
  popup.html
  popup.css
  popup.js
  icons/icon16.png
  icons/icon32.png
  icons/icon48.png
  icons/icon128.png
)

for f in manifest.json "${FILES[@]}"; do
  [ -f "$f" ] || { echo "missing: $f" >&2; exit 1; }
done

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

for f in "${FILES[@]}"; do
  mkdir -p "$STAGE/$(dirname "$f")"
  cp "$f" "$STAGE/$f"
done
node -e '
  const fs = require("fs");
  const m = JSON.parse(fs.readFileSync("manifest.json", "utf8"));
  delete m.key;
  fs.writeFileSync(process.argv[1], JSON.stringify(m, null, 2) + "\n");
' "$STAGE/manifest.json"
if [ -n "$KEY_PEM" ]; then
  cp "$KEY_PEM" "$STAGE/key.pem"
  OUT="dist/maiterm-chrome-${VERSION}-with-key.zip"
fi

rm -rf dist
mkdir -p dist
(cd "$STAGE" && zip -q -X -r "$ROOT/$OUT" .)

echo "$OUT"
# -Z1 lists bare entry names and is portable across BSD and GNU zip.
unzip -Z1 "$OUT" | sed 's/^/  /'
echo
echo "total: $(du -h "$OUT" | cut -f1)"
if [ -n "$KEY_PEM" ]; then
  echo
  echo "This zip holds the PRIVATE key. Upload it once, as the item's first package, then delete it."
fi
