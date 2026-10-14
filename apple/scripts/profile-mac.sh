#!/usr/bin/env bash
# Profiles the Mac app under load and prints where its time goes:
#
#   apple/scripts/profile-mac.sh [session|board] [seconds]
#
# Builds the app for release as a copy with its own bundle id (the installed app keeps running
# and keeps its pairings), starts `fake_daemon --busy`, drives the copy to the scene with the
# herder-macOS-Profile UI test (`session`: a long transcript whose turn keeps streaming, the
# default; `board`: the fleet with running agents), records it with Instruments' SwiftUI template
# for `seconds` (10 by default) and summarises the trace with trace-summary.py. Keeps the trace in
# target/profile/ to open in Instruments.
#
# The test drives the app with the keyboard and mouse: leave them alone until the recording
# starts. Needs what build-ffi.sh builds, XcodeGen, and Xcode with its Metal toolchain. The first
# run asks to let Xcode's test runner control the computer (Accessibility).
set -euo pipefail

cd "$(dirname "$0")/../.."
scene=${1:-session}
seconds=${2:-10}
out=$PWD/target/profile
derived=$out/DerivedData
app=$out/herder-profile.app
trace=$out/$scene-$(date +%Y%m%d-%H%M%S).trace
mkdir -p "$out"

cargo build -q -p herder-ffi --example fake_daemon
xcodegen generate --quiet --spec apple/project.yml
xcodebuild build -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-macOS -configuration Release -destination "platform=macOS" \
  -derivedDataPath "$derived" COMPILER_INDEX_STORE_ENABLE=NO
xcodebuild build-for-testing -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-macOS-Profile -destination "platform=macOS" -derivedDataPath "$derived" \
  COMPILER_INDEX_STORE_ENABLE=NO

rm -rf "$app"
cp -R "$derived/Build/Products/Release/herder.app" "$app"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier sh.herder.Herder.Profile" "$app/Contents/Info.plist"
codesign --force --deep --sign - --entitlements apple/App/herder.entitlements "$app"

# The daemons run until their stdin closes: when this script exits.
exec 3> >(target/debug/examples/fake_daemon --busy > "$out/fake_daemon.out")
for _ in $(seq 180); do [ -s "$out/fake_daemon.out" ] && break; sleep 1; done
[ -s "$out/fake_daemon.out" ] || { echo "the fake daemons did not start" >&2; exit 1; }

TEST_RUNNER_HERDER_PROFILE_APP=$app \
TEST_RUNNER_HERDER_PAIR_LINK=$(head -1 "$out/fake_daemon.out") \
TEST_RUNNER_HERDER_PROFILE_SCENE=$scene \
TEST_RUNNER_HERDER_PROFILE_SECONDS=$((seconds + 5)) \
  xcodebuild test-without-building -project apple/herder.xcodeproj -scheme herder-macOS-Profile \
  -destination "platform=macOS" -derivedDataPath "$derived" > "$out/test.log" 2>&1 &
test=$!

pid=
for _ in $(seq 180); do
  pid=$(sed -n 's/.*herder-profile-ready \([0-9][0-9]*\).*/\1/p' "$out/test.log" | head -1)
  [ -n "$pid" ] && break
  kill -0 "$test" 2>/dev/null || break
  sleep 1
done
[ -n "$pid" ] || { kill "$test" 2>/dev/null; tail -40 "$out/test.log" >&2; echo "the scene did not come up" >&2; exit 1; }
xcrun xctrace record --quiet --template SwiftUI --attach "$pid" --time-limit "${seconds}s" --output "$trace" \
  > "$out/xctrace.log" 2>&1
wait "$test" || { cat "$out/test.log" >&2; exit 1; }

echo "$scene, ${seconds}s: $trace"
python3 -I apple/scripts/trace-summary.py "$trace" "$pid" "$seconds"
