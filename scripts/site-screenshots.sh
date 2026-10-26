#!/usr/bin/env bash
# Regenerates the app screenshots on herder.sh (site/img/{mac,ios}-*.webp) from the
# `screenshots` workflow, which drives the Mac and iPhone apps against the demo fleet
# (apple/ScreenshotTests, `fake_daemon --demo`). The TUI's shots come from scripts/screenshots.sh.
#
# usage: scripts/site-screenshots.sh [<run id> | <dir>]
#   no argument  run the workflow on the current branch (pushed), wait for it, use its shots
#   <run id>     use the shots of that run of the workflow
#   <dir>        use the PNGs already in <dir>, such as a local run's exported attachments
#
# Needs gh and ImageMagick's magick. Each shot becomes a pair of webp files: Mac shots 1440 and
# 720 wide, iPhone shots 640 and 320.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
img="$root/site/img"
command -v magick >/dev/null || { echo "magick is missing: brew install imagemagick" >&2; exit 1; }

# site name <- the name ScreenshotTests gives the shot.
shots=(
    mac-hero:mac-tasks
    mac-failover:mac-failover
    mac-machines:mac-machines
    mac-agents:mac-task-agents
    mac-integrations:mac-integrations
    mac-backup:mac-machine-backup
    mac-member:mac-machine-member
    ios-board:ios-home
    ios-session:ios-session
    ios-request:ios-approval
)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

src=${1:-}
if [ -d "$src" ]; then
    dir=$src
else
    if [ -z "$src" ]; then
        branch=$(git -C "$root" branch --show-current)
        since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        gh workflow run screenshots.yml --ref "$branch"
        # The run shows up a few seconds after it is asked for.
        for _ in $(seq 30); do
            src=$(gh run list --workflow screenshots.yml --branch "$branch" --event workflow_dispatch \
                --created ">=$since" --limit 1 --json databaseId -q '.[0].databaseId')
            [ -n "$src" ] && break
            sleep 2
        done
        [ -n "$src" ] || { echo "the screenshots run did not start" >&2; exit 1; }
        echo "waiting for run $src"
        # A run that misses a screen still uploads the others; their names say what is missing.
        gh run watch "$src" --exit-status >/dev/null || echo "run $src failed; using the shots it took" >&2
    fi
    dir="$work/shots"
    gh run download "$src" --name screenshots --dir "$dir"
fi

missing=()
for pair in "${shots[@]}"; do
    name=${pair%%:*}
    png="$dir/${pair#*:}.png"
    if [ ! -f "$png" ]; then
        missing+=("${pair#*:}")
        continue
    fi
    case $name in
    mac-*)
        # A shot of the whole screen (a popover reaches past the window) keeps the window's
        # 1440×900, below the menu bar, plus room on the right for the popover.
        if [ "$(magick identify -format %w "$png")" -gt 1440 ]; then
            magick "$png" -crop 1600x900+0+40 +repage "$work/$name.png"
            png="$work/$name.png"
        fi
        large=1440 small=720
        ;;
    ios-*) large=640 small=320 ;;
    esac
    for width in $large $small; do
        magick "$png" -resize "${width}x" -quality 82 -define webp:method=6 "$img/$name-$width.webp"
    done
    echo "$name: $(du -ch "$img/$name"-*.webp | tail -1 | cut -f1)"
done

if [ ${#missing[@]} -gt 0 ]; then
    echo "missing: ${missing[*]}; their old images stay" >&2
    exit 1
fi
