#!/usr/bin/env bash
# Builds the Mac app for Release into target/<dir>/herder-profile.app as a copy with a bundle id
# of its own, sh.herder.Herder.Profile, so the installed app keeps running and keeps its
# pairings while profile-mac.sh and perf.sh drive the copy, and builds the UI tests of `scheme`
# that drive it:
#
#   apple/scripts/mac-release-copy.sh DIR SCHEME [xcodebuild settings...]
#
# The app builds and signs ad hoc, without the associated domains macOS refuses an ad-hoc app:
# the copy signs in with the fake daemons' recorded passkey. Settings after the scheme go to
# the tests' build, as an ad-hoc signature on CI.
set -euo pipefail

cd "$(dirname "$0")/../.."
out=$PWD/target/$1
scheme=$2
shift 2
derived=$out/DerivedData
app=$out/herder-profile.app
entitlements=$(apple/scripts/adhoc-entitlements.sh)
mkdir -p "$out"

xcodegen generate --quiet --spec apple/project.yml
xcodebuild build -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-macOS -configuration Release -destination "platform=macOS" \
  -derivedDataPath "$derived" COMPILER_INDEX_STORE_ENABLE=NO CODE_SIGN_IDENTITY=- \
  CODE_SIGNING_REQUIRED=NO HERDER_MAC_ENTITLEMENTS="$entitlements"
xcodebuild build-for-testing -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme "$scheme" -destination "platform=macOS" -derivedDataPath "$derived" \
  COMPILER_INDEX_STORE_ENABLE=NO "$@"

rm -rf "$app"
cp -R "$derived/Build/Products/Release/herder.app" "$app"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier sh.herder.Herder.Profile" "$app/Contents/Info.plist"
codesign --force --deep --sign - --entitlements "$entitlements" "$app"
