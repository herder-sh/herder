#!/usr/bin/env bash
# Measures how the apps launch, resume and scroll, and holds the scrolling to the budget:
#
#   apple/scripts/perf.sh [mac] [simulator] [iphone]     # the Mac alone by default
#
# Runs the measurements (apple/PerfTests) against fake daemons: `--busy`, the demo fleet plus a
# long transcript that keeps streaming, and `--load big 335 16000`, a machine with a used
# journal. On this Mac, all of them, against a Release copy of the app with its own bundle id
# (mac-release-copy.sh). On an iPhone simulator, the launches alone: its frames say nothing of
# a phone's. On the iPhone whose UDID is in `HERDER_PERF_DEVICE`, the scrolling alone: the
# machine with the journal listens on this Mac only. Both build the app for Release as
# sh.herder.Herder.Perf, so the installed app stays.
#
# Writes target/perf/perf.md: each screen's hitch time ratio, when each launch showed the
# Board, and the main thread's hangs while each test measured, which the app logs
# (Hangs.swift) and the Mac and the simulator let the script read. Fails when a test failed, or
# a screen is over the budget, 5 ms/s, by more than `PERF_TOLERANCE` ms/s (0 by default).
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
# settings to the Mac tests' build, as CI's ad-hoc signature. `PERF_ONLY` names the tests to
# run, e.g. `PERF_ONLY="Providers Launch"`; by default, every one the platform can.
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
  daemon=$PWD/target/debug/examples/fake_daemon
fi
# A fake daemon stopped for a test runs again, to end when this script does.
trap 'pkill -CONT -f "$daemon --" 2> /dev/null || true' EXIT

# Waits `seconds` at most for the fake daemon writing `log` to print its link and passkey.
started() {
  for _ in $(seq "$2"); do [ "$(wc -l < "$1")" -ge 2 ] && return; sleep 1; done
  echo "the fake daemons did not start" >&2
  exit 1
}

# Starts `fake_daemon --busy` on `address`, which runs until its stdin closes: when this script
# exits, or the next platform's starts. Sets `link` and `passkey` to what it prints.
busy() {
  local log=$out/fake_daemon-busy.out
  : > "$log"
  exec 3> >(FAKE_DAEMON_ADDRESS=$1 "$daemon" --busy > "$log")
  started "$log" 300
  link=$(head -1 "$log")
  passkey=$(sed -n 2p "$log")
}

# The tests to run, each as <class>/<test>: PERF_ONLY's, or with none every one the platform
# can. LaunchUnreachable runs apart, on the profile Launch leaves: asking for it runs both.
tests=()
unreachable=yes
if [ -n "${PERF_ONLY:-}" ]; then
  unreachable=no
  launch=no
  for name in $PERF_ONLY; do
    case $name in
    LaunchUnreachable) unreachable=yes launch=yes ;;
    Launch) launch=yes ;;
    OpenLongSession | Resume) tests+=("LaunchPerfTests/test$name") ;;
    *) tests+=("ScrollPerfTests/test$name") ;;
    esac
  done
  [ "$launch" = no ] || tests+=(LaunchPerfTests/testLaunch)
fi

# Runs the tests of the scheme herder-<target> for `platform`, with the xcodebuild arguments
# after them: PERF_ONLY's, or with none those of `class`, or of every class when it is empty.
# First all but LaunchUnreachable, into target/perf/<platform>.xcresult; then that one, with
# the fake daemons stopped so that no machine answers, into <platform>-unreachable.xcresult.
measure() {
  local platform=$1 target=$2 only=()
  [ -z "$3" ] || only=("$3")
  [ ${#tests[@]} -eq 0 ] || only=("${tests[@]}")
  shift 3
  local apart=herder-$target/LaunchPerfTests/testLaunchUnreachable
  rm -rf "$out/$platform.xcresult" "$out/$platform-unreachable.xcresult"
  xcodebuild test-without-building -project apple/herder.xcodeproj -scheme "herder-$target" "$@" \
    -resultBundlePath "$out/$platform.xcresult" "-skip-testing:$apart" \
    ${only[@]+"${only[@]/#/-only-testing:herder-$target/}"} > "$out/$platform-test.log" 2>&1 || true
  results+=("$platform=$out/$platform.xcresult")
  [ "$unreachable" = yes ] || return 0
  pkill -STOP -f "$daemon --" || { echo "no fake daemon to stop" >&2; exit 1; }
  xcodebuild test-without-building -project apple/herder.xcodeproj -scheme "herder-$target" "$@" \
    -resultBundlePath "$out/$platform-unreachable.xcresult" "-only-testing:$apart" \
    >> "$out/$platform-test.log" 2>&1 || true
  pkill -CONT -f "$daemon --"
  results+=("$platform=$out/$platform-unreachable.xcresult")
}

# The machine with a used journal, for the Mac and the simulator. Writing the journal takes a
# while, so it starts before the builds; `loaded` waits for it and sets `load_link`.
case " ${platforms[*]} " in
*" mac "* | *" simulator "*)
  : > "$out/fake_daemon-load.out"
  exec 4> >("$daemon" --load big 335 16000 > "$out/fake_daemon-load.out")
  ;;
esac
loaded() {
  started "$out/fake_daemon-load.out" 900
  load_link=$(head -1 "$out/fake_daemon-load.out")
}

results=()
logs=()
for platform in "${platforms[@]}"; do
  case $platform in
  mac)
    apple/scripts/mac-release-copy.sh perf herder-macOS-Perf ${PERF_MAC_SETTINGS:-}
    busy 127.0.0.1
    loaded
    export TEST_RUNNER_HERDER_PERF_APP=$out/herder-profile.app
    export TEST_RUNNER_HERDER_PERF_PROFILE=perf-$(date +%s)
    export TEST_RUNNER_HERDER_PAIR_LINK=$link TEST_RUNNER_HERDER_LOAD_LINK=$load_link
    export TEST_RUNNER_HERDER_PASSKEY=$passkey
    since=$(date '+%Y-%m-%d %H:%M:%S')
    measure mac macOS-Perf "" -destination "platform=macOS" -derivedDataPath "$out/DerivedData"
    # The copy's lines, and the tests' own, which say where each test measured.
    /usr/bin/log show --start "$since" --style ndjson --predicate \
      'subsystem == "sh.herder" AND (category == "perf" OR processImagePath CONTAINS "herder-profile.app")' \
      > "$out/mac.log" || true
    logs+=(--log "mac=$out/mac.log")
    ;;
  simulator)
    device=$(xcrun simctl list devices available -j |
      jq -r '[.devices[][] | select(.name | startswith("iPhone"))][0].udid')
    # Booted while the app builds; an error says it was booted already.
    xcrun simctl boot "$device" 2> /dev/null || true
    xcodegen generate --quiet --spec apple/project.yml
    xcodebuild build-for-testing -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
      -scheme herder-iOS-Perf -configuration Release -destination "platform=iOS Simulator,id=$device" \
      -derivedDataPath "$out/DerivedData-simulator" COMPILER_INDEX_STORE_ENABLE=NO ONLY_ACTIVE_ARCH=YES \
      HERDER_IOS_BUNDLE_ID=sh.herder.Herder.Perf
    xcrun simctl bootstatus "$device" > /dev/null
    busy 127.0.0.1
    loaded
    export TEST_RUNNER_HERDER_PERF_PROFILE=perf-$(date +%s)
    export TEST_RUNNER_HERDER_PAIR_LINK=$link TEST_RUNNER_HERDER_LOAD_LINK=$load_link
    export TEST_RUNNER_HERDER_PASSKEY=$passkey
    since=$(date '+%Y-%m-%d %H:%M:%S')
    measure simulator iOS-Perf LaunchPerfTests -configuration Release \
      -destination "platform=iOS Simulator,id=$device" -derivedDataPath "$out/DerivedData-simulator"
    xcrun simctl spawn "$device" log show --start "$since" --style ndjson \
      --predicate 'subsystem == "sh.herder"' > "$out/simulator.log" || true
    logs+=(--log "simulator=$out/simulator.log")
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
    only=(${tests[@]+"${tests[@]}"})
    [ ${#only[@]} -gt 0 ] || only=(ScrollPerfTests)
    rm -rf "$out/iphone.xcresult"
    TEST_RUNNER_HERDER_PERF_PROFILE=perf-$(date +%s) \
    TEST_RUNNER_HERDER_PAIR_LINK=$link \
    TEST_RUNNER_HERDER_PASSKEY=$passkey \
      xcodebuild test-without-building -project apple/herder.xcodeproj -scheme herder-iOS-Perf \
      -configuration Release -destination "id=$device" -derivedDataPath "$out/DerivedData-iphone" \
      -resultBundlePath "$out/iphone.xcresult" "${only[@]/#/-only-testing:herder-iOS-Perf/}" \
      > "$out/iphone-test.log" 2>&1 || true
    results+=("iphone=$out/iphone.xcresult")
    ;;
  *)
    echo "unknown platform $platform: mac, simulator or iphone" >&2
    exit 2
    ;;
  esac
done

python3 -I apple/scripts/perf-report.py --budget 5 --tolerance "${PERF_TOLERANCE:-0}" \
  ${logs[@]+"${logs[@]}"} "${results[@]}" | tee "$out/perf.md"
