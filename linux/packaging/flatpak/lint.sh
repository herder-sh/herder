#!/bin/sh
# Lints the manifest, and the repo flatpak-builder exported it to when given one, with
# flatpak-builder-lint as Flathub runs it. The repo passes only when built with
# --mirror-screenshots-url=https://dl.flathub.org/media --compose-url-policy=full, as Flathub
# builds it (README.md).
#   linux/packaging/flatpak/lint.sh [REPO]
set -eu

here=$(cd "$(dirname "$0")" && pwd)
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user -y --noninteractive flathub org.flatpak.Builder >/dev/null
lint() { flatpak run --command=flatpak-builder-lint org.flatpak.Builder "$@"; }

lint manifest "$here/sh.herder.Herder.yml"
if [ $# -gt 0 ]; then
  lint repo "$1"
fi
