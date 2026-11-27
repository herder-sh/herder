#!/bin/sh
# Copies the flatpak installations the CI container image ships into the container's own
# writable layer. On some runner images Docker's overlay storage cannot rename the image's
# directories, so flatpak-builder's runtime update fails with "Invalid cross-device link"
# (flatpak/flatpak-github-actions#267); a copy lives wholly in the upper layer.
set -eu

for dir in /var/lib/flatpak "$HOME/.local/share/flatpak"; do
  [ -d "$dir" ] || continue
  cp -a "$dir" "$dir.copy"
  rm -rf "$dir"
  mv "$dir.copy" "$dir"
  echo "copied $dir into the container layer"
done
