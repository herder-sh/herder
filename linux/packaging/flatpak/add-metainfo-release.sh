#!/bin/sh
# Adds a <release> entry for VERSION, dated DATE (YYYY-MM-DD), at the top of the metainfo's
# <releases>, unless it has one already. The release workflow runs it before building the
# Flatpak, so a release never waits on a hand edit of the metainfo.
#   linux/packaging/flatpak/add-metainfo-release.sh VERSION DATE
set -eu

version=$1
date=$2
metainfo="$(cd "$(dirname "$0")/../../data" && pwd)/sh.herder.Herder.metainfo.xml"

if grep -q "<release version=\"$version\"" "$metainfo"; then
  echo "the metainfo already has release $version"
  exit 0
fi
url="https://github.com/herder-sh/herder/releases/tag/v$version"
sed -i "s|^\( *\)<releases>\$|&\n\1  <release version=\"$version\" date=\"$date\">\n\1    <url type=\"details\">$url</url>\n\1  </release>|" "$metainfo"
grep -q "<release version=\"$version\" date=\"$date\">" "$metainfo"
echo "added release $version ($date) to the metainfo"
