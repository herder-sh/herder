#!/usr/bin/env bash
# Runs the Mac UI tests (apple/MacUITests) against the demo fleet:
#
#   apple/scripts/test-mac-ui.sh [-only-testing:herder-macOS-UITests/<Class>/<test> ...]
#
# Builds the app as a copy with its own bundle id (the installed app keeps running and keeps its
# pairings), starts `fake_daemon --demo` and runs the tests on the copy; extra arguments go to
# `xcodebuild test-without-building`. Keeps the result bundle in target/mac-ui-tests/.
#
# The tests drive the app with the keyboard and mouse, and use the pasteboard: leave them alone
# while they run. Needs what build-ffi.sh builds, XcodeGen, and Xcode with its Metal toolchain.
# HERDER_FAKE_DAEMON names a fake daemon built already, as CI's cache holds one; otherwise the
# script builds it.
# The first run asks to let Xcode's test runner control the computer (Accessibility).
set -euo pipefail

cd "$(dirname "$0")/../.."
out=$PWD/target/mac-ui-tests
derived=$out/DerivedData
app=$out/herder-ui-tests.app
mkdir -p "$out"
entitlements=$(apple/scripts/adhoc-entitlements.sh)

daemon=${HERDER_FAKE_DAEMON:-}
if [ -z "$daemon" ]; then
  cargo build -q -p herder-ffi --example fake_daemon
  daemon=target/debug/examples/fake_daemon
fi
xcodegen generate --quiet --spec apple/project.yml
xcodebuild build -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-macOS -configuration Debug -destination "platform=macOS" \
  -derivedDataPath "$derived" COMPILER_INDEX_STORE_ENABLE=NO CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY=- \
  DEVELOPMENT_TEAM= CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO HERDER_MAC_ENTITLEMENTS="$entitlements"
xcodebuild build-for-testing -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-macOS-UITests -destination "platform=macOS" -derivedDataPath "$derived" \
  COMPILER_INDEX_STORE_ENABLE=NO

rm -rf "$app"
cp -R "$derived/Build/Products/Debug/herder.app" "$app"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier sh.herder.Herder.UITestsApp" "$app/Contents/Info.plist"
codesign --force --deep --sign - --entitlements "$entitlements" "$app"

# The daemons run until their stdin closes: when this script exits.
exec 3> >(env -u GIT_CONFIG_COUNT -u HERDER_SESSION_ID "$daemon" --demo > "$out/fake_daemon.out")
for _ in $(seq 180); do [ "$(wc -l < "$out/fake_daemon.out")" -ge 2 ] && break; sleep 1; done
[ "$(wc -l < "$out/fake_daemon.out")" -ge 2 ] || { echo "the fake daemons did not start" >&2; exit 1; }

rm -rf "$out/tests.xcresult"
TEST_RUNNER_HERDER_APP=$app \
TEST_RUNNER_HERDER_PAIR_LINK=$(head -1 "$out/fake_daemon.out") \
TEST_RUNNER_HERDER_PASSKEY=$(sed -n 2p "$out/fake_daemon.out") \
  xcodebuild test-without-building -project apple/herder.xcodeproj -scheme herder-macOS-UITests \
  -destination "platform=macOS" -derivedDataPath "$derived" -resultBundlePath "$out/tests.xcresult" "$@"
