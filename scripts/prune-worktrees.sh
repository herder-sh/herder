#!/usr/bin/env bash
# Removes every worktree of this repository whose branch's PR is merged or closed. Each
# worktree's target/ holds 10-25 GB, so finished ones fill the disk fast. Branches are kept.
# Left alone: the main checkout, detached worktrees, worktrees with uncommitted or untracked
# changes, and herder session worktrees (herder removes them itself).
#
# usage: scripts/prune-worktrees.sh [--dry-run]
set -euo pipefail

dry_run=${1:-}
root=$(cd "$(dirname "$0")/.." && pwd)
main=$(git -C "$root" worktree list --porcelain | sed -n '1s/^worktree //p')
states=$(gh pr list --repo herder-sh/herder --state all --limit 1000 \
    --json number,state,headRefName \
    --jq 'sort_by(.number) | map({(.headRefName): .state}) | add | to_entries[] | "\(.key) \(.value)"')

git -C "$root" worktree list --porcelain | awk '
    /^worktree / { path = substr($0, 10) }
    /^branch / { print path "\t" substr($2, 12) }
' | while IFS=$'\t' read -r path branch; do
    [ "$path" = "$main" ] && continue
    state=$(awk -v b="$branch" '$1 == b { print $2 }' <<<"$states")
    [ "$state" = MERGED ] || [ "$state" = CLOSED ] || continue
    # herder points a session's core.hooksPath at <data>/hooks/<session> and keeps its
    # worktree in <data>/worktrees. A worktree added from inside one copies the setting, so
    # the path is what tells them apart.
    hooks=$(git -C "$path" config --worktree --get core.hooksPath 2>/dev/null || true)
    if [ -n "$hooks" ] && [[ "$path" == "$(dirname "$(dirname "$hooks")")/worktrees/"* ]]; then
        continue
    fi
    if [ -n "$(git -C "$path" status --porcelain)" ]; then
        echo "kept $path: $branch is $state but has uncommitted changes" >&2
        continue
    fi
    echo "removing $path ($branch, $state)"
    [ "$dry_run" = --dry-run ] || git -C "$main" worktree remove --force "$path"
done
git -C "$main" worktree prune
