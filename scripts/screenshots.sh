#!/usr/bin/env bash
# Renders the TUI's screenshot scenes (crates/herder-tui/src/views/screenshots.rs) to
# docs/screenshots/<todo>/ with Charm's freeze: each scene at 45, 100 and 160 columns, in the
# dark and light herder themes.
#
# usage: scripts/screenshots.sh <todo> [scene,scene,...]
#   e.g. scripts/screenshots.sh p2d-2 components,dialog
set -euo pipefail

todo=${1:?usage: scripts/screenshots.sh <todo> [scene,scene,...]}
root=$(cd "$(dirname "$0")/.." && pwd)
if ! command -v freeze >/dev/null; then
    echo "freeze is missing: install a release from https://github.com/charmbracelet/freeze/releases" >&2
    exit 1
fi

HERDER_SCREENSHOTS="$root/docs/screenshots/$todo" HERDER_SCENES="${2:-}" \
    cargo test -p herder-tui --lib -- --ignored --exact views::screenshots::screenshots

# freeze renders large; half that, in 128 colours, reads as well at a tenth of the size.
if command -v magick >/dev/null; then
    shopt -s nullglob
    for png in "$root/docs/screenshots/$todo"/*-{45,100,160}-{dark,light}.png; do
        magick "$png" -resize 50% -dither None -colors 128 "PNG8:$png"
    done
else
    echo "magick is missing: screenshots stay full size" >&2
fi
