#!/usr/bin/env bash
set -euo pipefail

# Regenerates assets/icon/*.png and assets/icon/caracal.ico from the master
# SVG (assets/icon/caracal-mark.svg). Requires `rsvg-convert` (librsvg) and
# ImageMagick's `magick` on PATH. Run from anywhere; outputs are checked
# into git and are NOT regenerated in CI — only re-run this after editing
# the master SVG.

cd "$(dirname "$0")/.."

SRC="assets/icon/caracal-mark.svg"
OUT="assets/icon"

for size in 16 24 32 48 128 256 512; do
  rsvg-convert -w "$size" -h "$size" "$SRC" -o "$OUT/icon-$size.png"
done

# Windows .ico max standard frame size is 256x256 — 512 isn't included here.
magick "$OUT/icon-16.png" "$OUT/icon-24.png" "$OUT/icon-32.png" \
       "$OUT/icon-48.png" "$OUT/icon-256.png" "$OUT/caracal.ico"

echo "Generated:"
ls -la "$OUT"/icon-*.png "$OUT/caracal.ico"
