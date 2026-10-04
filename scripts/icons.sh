#!/usr/bin/env bash
# Renders the Apple app icons from assets/icon/. Needs rsvg-convert and ImageMagick.
set -euo pipefail

cd "$(dirname "$0")/.."
src=assets/icon
apple=apple/App/Assets.xcassets/AppIcon.appiconset

# iOS rejects an icon with an alpha channel.
rsvg-convert -w 1024 -h 1024 "$src/herder.svg" | magick - -background '#111216' -alpha remove \
  -alpha off "$apple/icon-ios-1024.png"
rsvg-convert -w 512 -h 512 "$src/herder-rounded.svg" -o "$apple/icon-mac-512.png"
rsvg-convert -w 1024 -h 1024 "$src/herder-rounded.svg" -o "$apple/icon-mac-1024.png"
