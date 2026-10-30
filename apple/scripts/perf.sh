#!/usr/bin/env bash
# Measures how smoothly the apps scroll and holds them to the budget:
#
#   apple/scripts/perf.sh [mac] [iphone]     # the Mac alone by default
#
# Runs the scroll measurements (apple/PerfTests) against `fake_daemon --busy`, the demo fleet
# plus a long transcript that keeps streaming: on this Mac, against a Release copy of the app
# with its own bundle id (mac-release-copy.sh); on the iPhone whose UDID is in
# `HERDER_PERF_DEVICE`, against the app built for Release as sh.herder.Herder.Perf, so the
# installed app stays. Writes target/perf/perf.md, each screen's hitch time ratio, and fails
# when one is over the budget, 5 ms/s, by more than `PERF_TOLERANCE` ms/s (0 by default).
#
# The iPhone build signs with `DEVELOPMENT_TEAM` (WE8DRASDBR by default), through the App Store
# Connect key in ASC_KEY_ID, ASC_ISSUER_ID and ASC_PRIVATE_KEY when they are set, as `herder
# secret run` sets them. The iPhone reaches its fake daemons on an address of this Mac's:
# `FAKE_DAEMON_ADDRESS`, by default its Tailscale address when both are on the tailnet, as a
# hotspot or a guest network may keep the phone from the Mac's own, else en0's.
#
# The Mac tests drive the copy with the mouse: leave it alone while they run. Needs what
# build-ffi.sh builds, XcodeGen, and Xcode with its Metal toolchain. The first run asks to let
# Xcode's test runner control the computer (Accessibility). `PERF_MAC_SETTINGS` passes build
# settings to the Mac tests' build, as CI's ad-hoc signature.
set -euo pipefail

cd "$(dirname "$0")/../.."
platforms=("$@")
[ ${#platforms[@]} -gt 0 ] || platforms=(mac)
out=$PWD/target/perf
mkdir -p "$out"
# The fake daemon at HERDER_FAKE_DAEMON, as CI caches it, or built here.
daemon=${HERDER_FAKE_DAEMON:-}
if [ -z "$daemon" ]; then
  cargo build -q -p herder-ffi --example fake_daemon
  daemon=target/debug/examples/fake_daemon
fi

# Starts `fake_daemon --busy` on `address`, which runs until its stdin closes: when this script
# exits, or the next platform's starts. Sets `link` and `passkey` to what it prints.
busy() {
  local log=$out/fake_daemon-$1.out
  exec 3> >(FAKE_DAEMON_ADDRESS=$1 "$daemon" --busy > "$log")
  for _ in $(seq 300); do [ "$(wc -l < "$log")" -ge 2 ] && break; sleep 1; done
  [ "$(wc -l < "$log")" -ge 2 ] || { echo "the fake daemons did not start" >&2; exit 1; }
  link=$(head -1 "$log")
  passkey=$(sed -n 2p "$log")
}

results=()
for platform in "${platforms[@]}"; do
  result=$out/$platform.xcresult
  rm -rf "$result"
  case $platform in
  mac)
    apple/scripts/mac-release-copy.sh perf herder-macOS-Perf ${PERF_MAC_SETTINGS:-}
    busy 127.0.0.1
    TEST_RUNNER_HERDER_PERF_APP=$out/herder-profile.app \
    TEST_RUNNER_HERDER_PERF_PROFILE=perf-$(date +%s) \
    TEST_RUNNER_HERDER_PAIR_LINK=$link \
    TEST_RUNNER_HERDER_PASSKEY=$passkey \
      xcodebuild test-without-building -project apple/herder.xcodeproj -scheme herder-macOS-Perf \
      -destination "platform=macOS" -derivedDataPath "$out/DerivedData" -resultBundlePath "$result" \
      > "$out/mac-test.log" 2>&1 || true
    ;;
  iphone)
    device=${HERDER_PERF_DEVICE:?the iPhone to measure on: its UDID in HERDER_PERF_DEVICE}
    signing=(DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-WE8DRASDBR}" CODE_SIGN_STYLE=Automatic -allowProvisioningUpdates)
    if [ -n "${ASC_KEY_ID:-}" ]; then
      key_id=$(printf %s "$ASC_KEY_ID" | tr -d '[:space:]')
      key=$(mktemp -d)/AuthKey_$key_id.p8
      (umask 077 && printf '%s\n' "$ASC_PRIVATE_KEY" > "$key")
      signing+=(-authenticationKeyPath "$key" -authenticationKeyID "$key_id"
        -authenticationKeyIssuerID "$(printf %s "$ASC_ISSUER_ID" | tr -d '[:space:]')")
    fi
    xcodegen generate --quiet --spec apple/project.yml
    xcodebuild build-for-testing -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
      -scheme herder-iOS-Perf -configuration Release -destination "id=$device" \
      -derivedDataPath "$out/DerivedData-iphone" COMPILER_INDEX_STORE_ENABLE=NO \
      HERDER_IOS_BUNDLE_ID=sh.herder.Herder.Perf "${signing[@]}"
    [ -z "${key:-}" ] || rm -rf "$(dirname "$key")"
    busy "${FAKE_DAEMON_ADDRESS:-$(tailscale ip -4 2>/dev/null || ipconfig getifaddr en0)}"
    TEST_RUNNER_HERDER_PERF_PROFILE=perf-$(date +%s) \
    TEST_RUNNER_HERDER_PAIR_LINK=$link \
    TEST_RUNNER_HERDER_PASSKEY=$passkey \
      xcodebuild test-without-building -project apple/herder.xcodeproj -scheme herder-iOS-Perf \
      -configuration Release -destination "id=$device" -derivedDataPath "$out/DerivedData-iphone" \
      -resultBundlePath "$result" > "$out/iphone-test.log" 2>&1 || true
    ;;
  *)
    echo "unknown platform $platform: mac or iphone" >&2
    exit 2
    ;;
  esac
  results+=("$platform=$result")
done

python3 -I apple/scripts/perf-report.py --budget 5 --tolerance "${PERF_TOLERANCE:-0}" "${results[@]}" \
  | tee "$out/perf.md"
