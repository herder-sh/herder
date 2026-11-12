#!/usr/bin/env bash
# Builds the Mac app in Release, signed by the team, and installs it as /Applications/herder.app.
# Unlike the ad-hoc zip build-mac-release.sh makes without the signing secrets, this build has an
# application identifier and the herder.sh associated domains, which macOS requires before it
# lets the app use your passkey: an ad-hoc copy fails every approval (creating a secret set, inviting, claiming) with
# "The calling process does not have an application identifier".
#
#   apple/scripts/install-mac.sh
#
# Signs with DEVELOPMENT_TEAM (WE8DRASDBR by default); the team's development profile must list
# this Mac. The app replaced is kept as target/install-mac/herder-previous.app. Needs Xcode,
# XcodeGen and the aarch64-apple-darwin Rust target.
set -euo pipefail

cd "$(dirname "$0")/../.."
out=$PWD/target/install-mac
derived=$out/DerivedData
installed=/Applications/herder.app
mkdir -p "$out"

apple/scripts/build-ffi.sh aarch64-apple-darwin
xcodegen generate --quiet --spec apple/project.yml
xcodebuild build -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-macOS -configuration Release -destination "generic/platform=macOS" \
  -derivedDataPath "$derived" COMPILER_INDEX_STORE_ENABLE=NO CODE_SIGN_STYLE=Automatic \
  DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-WE8DRASDBR}" -allowProvisioningUpdates

app="$derived/Build/Products/Release/herder.app"
codesign --verify --strict --deep "$app"
codesign -d --entitlements - "$app" 2>/dev/null | grep -q webcredentials:herder.sh || {
  echo "the build has no herder.sh associated domains, so passkeys would fail" >&2
  exit 1
}

osascript -e 'quit app id "sh.herder.Herder"' 2>/dev/null || true
if [ -d "$installed" ]; then
  rm -rf "$out/herder-previous.app"
  mv "$installed" "$out/herder-previous.app"
fi
ditto "$app" "$installed"
open "$installed"
echo "installed $installed"
