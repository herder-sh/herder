#!/usr/bin/env bash
# Fails unless a built herder.app carries its privacy manifests, which App Store Connect asks
# for: the app's own (apple/App/PrivacyInfo.xcprivacy) and HerderKit's, in HerderKit's
# resource bundle.
#
#   apple/scripts/check-privacy-manifest.sh <path to herder.app>
set -euo pipefail

app=${1:?usage: check-privacy-manifest.sh <herder.app>}
# An iOS bundle is flat; a macOS one keeps its resources in Contents/Resources.
resources=$app
[ -d "$app/Contents/Resources" ] && resources=$app/Contents/Resources
for manifest in "$resources/PrivacyInfo.xcprivacy" \
  "$resources/HerderKit_HerderKit.bundle/PrivacyInfo.xcprivacy" \
  "$resources/HerderKit_HerderKit.bundle/Contents/Resources/PrivacyInfo.xcprivacy"; do
  if [ -f "$manifest" ]; then plutil -lint "$manifest"; fi
done
[ -f "$resources/PrivacyInfo.xcprivacy" ] || {
  echo "$app has no PrivacyInfo.xcprivacy" >&2
  exit 1
}
[ -f "$resources/HerderKit_HerderKit.bundle/PrivacyInfo.xcprivacy" ] ||
  [ -f "$resources/HerderKit_HerderKit.bundle/Contents/Resources/PrivacyInfo.xcprivacy" ] || {
  echo "$app has no HerderKit_HerderKit.bundle/PrivacyInfo.xcprivacy" >&2
  find "$resources" -maxdepth 3 -name '*.xcprivacy' >&2
  exit 1
}
