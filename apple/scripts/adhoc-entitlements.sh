#!/usr/bin/env bash
# Prints the path of a copy of the Mac app's entitlements fit for an ad-hoc signature: without
# associated domains (the herder passkey's webcredentials:herder.sh), which need a provisioning
# profile; macOS refuses to launch an ad-hoc app that claims them. Passkeys then work only in a
# team-signed build. Pass it to xcodebuild as HERDER_MAC_ENTITLEMENTS, which only the app target
# reads: CODE_SIGN_ENTITLEMENTS on the command line would sandbox SwiftTerm's build-info tool too.
set -euo pipefail

cd "$(dirname "$0")/.."
entitlements="$(mktemp -d)/herder.entitlements"
cp App/herder.entitlements "$entitlements"
/usr/libexec/PlistBuddy -c "Delete :com.apple.developer.associated-domains" "$entitlements"
echo "$entitlements"
