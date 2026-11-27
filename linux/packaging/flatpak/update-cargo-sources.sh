#!/bin/sh
# Writes cargo-sources.json, the crates the Flatpak build vendors, from Cargo.lock with
# flatpak-cargo-generator at a pinned commit. CI runs it before every Flatpak build; run it
# before a local build, and for a Flathub update at its tag. The file is not committed. Needs uv.
set -eu

GENERATOR_COMMIT=41c20aa10819cdb2a4f3ca171758a96d1955c018
GENERATOR_SHA256=0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL -o "$tmp/flatpak-cargo-generator.py" \
  "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$GENERATOR_COMMIT/cargo/flatpak-cargo-generator.py"
echo "$GENERATOR_SHA256  $tmp/flatpak-cargo-generator.py" | sha256sum -c --quiet -
uv run --quiet --script "$tmp/flatpak-cargo-generator.py" \
  "$root/Cargo.lock" -o "$here/cargo-sources.json"
