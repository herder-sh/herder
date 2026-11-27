#!/bin/sh
# Fails unless the AppStream metainfo's newest <release> is the workspace version, so every
# release ships its release entry to software centres and Flathub.
set -eu

root=$(cd "$(dirname "$0")/../../.." && pwd)
metainfo="$root/linux/data/sh.herder.Herder.metainfo.xml"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
newest=$(sed -n 's/.*<release version="\([^"]*\)".*/\1/p' "$metainfo" | head -n 1)

if [ "$newest" != "$version" ]; then
  echo "::error file=linux/data/sh.herder.Herder.metainfo.xml::the newest <release> is $newest, not the workspace version $version: add <release version=\"$version\" date=\"YYYY-MM-DD\"> above it"
  exit 1
fi
echo "the metainfo's newest release is $version"
