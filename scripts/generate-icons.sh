#!/usr/bin/env bash
# Generate distribution icons from an SVG: PNGs for Linux (hicolor sizes),
# a multi-size .ico for Windows, and a retina-complete .icns for macOS.
# Requires rsvg-convert, magick (ImageMagick 7), and uvx (for icnsutil).
set -euo pipefail

usage() {
    echo "usage: $(basename "$0") <icon.svg> <output-dir>" >&2
    exit 1
}

[[ $# -eq 2 ]] || usage
SVG="$1"
OUT="$2"

[[ -f "$SVG" ]] || { echo "error: no such file: $SVG" >&2; exit 1; }
for tool in rsvg-convert magick uvx; do
    command -v "$tool" >/dev/null || { echo "error: $tool not found" >&2; exit 1; }
done

mkdir -p "$OUT"
ICONSET="$(mktemp -d)/icon.iconset"
mkdir -p "$ICONSET"
trap 'rm -rf "$(dirname "$ICONSET")"' EXIT

for size in 16 24 32 48 64 128 256 512 1024; do
    rsvg-convert -w "$size" -h "$size" "$SVG" -o "$OUT/icon_${size}.png"
done

# macOS iconset: each point size at 1x and 2x, mapped from the plain renders.
cp "$OUT/icon_16.png"   "$ICONSET/icon_16x16.png"
cp "$OUT/icon_32.png"   "$ICONSET/icon_16x16@2x.png"
cp "$OUT/icon_32.png"   "$ICONSET/icon_32x32.png"
cp "$OUT/icon_64.png"   "$ICONSET/icon_32x32@2x.png"
cp "$OUT/icon_128.png"  "$ICONSET/icon_128x128.png"
cp "$OUT/icon_256.png"  "$ICONSET/icon_128x128@2x.png"
cp "$OUT/icon_256.png"  "$ICONSET/icon_256x256.png"
cp "$OUT/icon_512.png"  "$ICONSET/icon_256x256@2x.png"
cp "$OUT/icon_512.png"  "$ICONSET/icon_512x512.png"
cp "$OUT/icon_1024.png" "$ICONSET/icon_512x512@2x.png"
uvx icnsutil compose -f "$OUT/icon.icns" "$ICONSET"/*.png

magick "$OUT/icon_16.png" "$OUT/icon_24.png" "$OUT/icon_32.png" \
    "$OUT/icon_48.png" "$OUT/icon_64.png" "$OUT/icon_128.png" \
    "$OUT/icon_256.png" "$OUT/icon.ico"

cp "$SVG" "$OUT/icon.svg"

echo "generated in $OUT: icon_{16,24,32,48,64,128,256,512,1024}.png icon.icns icon.ico"
