#!/usr/bin/env bash
# Times the iPhone app's launch to its cached Board on a real device, in Release, and fails when
# it is over budget:
#
#   apple/scripts/launch-ios.sh <device udid> [launches] [budget ms]
#
# Builds the app for release as a copy with its own bundle id, sh.herder.Herder.Perf, so the
# installed app and its pairings are left alone. The copy's profile is the demo fleet's
# (`fake_daemon --demo`), signed in to in the simulator and copied over with its offline cache.
# On the device the demo's machines, on this Mac's localhost, never answer: the Board shows from
# the cache alone, as when every machine is asleep. Each launch prints when each step came, in
# ms since the process started (see Launch.swift); the first launch after the install is cold.
# Fails when the median launch shows the Board, its rows drawn and the splash gone, after
# `budget` ms (1000 by default).
#
# Signs with DEVELOPMENT_TEAM (WE8DRASDBR by default); with ASC_KEY_ID, ASC_ISSUER_ID and
# ASC_PRIVATE_KEY set, xcodebuild registers the copy's bundle id through that App Store Connect
# key. The device must be unlocked. Needs what build-ffi.sh builds and XcodeGen.
set -euo pipefail

cd "$(dirname "$0")/../.."
device=${1:?usage: launch-ios.sh <device udid> [launches] [budget ms]}
launches=${2:-10}
budget=${3:-1000}
bundle=sh.herder.Herder.Perf
out=$PWD/target/launch-ios
derived=$out/DerivedData
mkdir -p "$out"

signing=(DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-WE8DRASDBR}" -allowProvisioningUpdates)
if [ -n "${ASC_KEY_ID:-}" ]; then
  key_dir=$(mktemp -d)
  trap 'rm -rf "$key_dir"' EXIT
  key_id=$(printf %s "$ASC_KEY_ID" | tr -d '[:space:]')
  (umask 077; printf '%s\n' "$ASC_PRIVATE_KEY" > "$key_dir/AuthKey_$key_id.p8")
  signing+=(-authenticationKeyPath "$key_dir/AuthKey_$key_id.p8" -authenticationKeyID "$key_id"
    -authenticationKeyIssuerID "$(printf %s "$ASC_ISSUER_ID" | tr -d '[:space:]')")
fi

cargo build -q -p herder-ffi --example fake_daemon
xcodegen generate --quiet --spec apple/project.yml
for platform in "iOS Simulator" iOS; do
  extra=()
  [ "$platform" = iOS ] && extra=("${signing[@]}")
  xcodebuild build -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
    -scheme herder-iOS -configuration Release -destination "generic/platform=$platform" -derivedDataPath "$derived" \
    PRODUCT_BUNDLE_IDENTIFIER="$bundle" COMPILER_INDEX_STORE_ENABLE=NO ${extra[@]+"${extra[@]}"}
done

# The profile: the demo fleet signed in to in a simulator of its own, held until the client has
# saved its offline cache (every 30 s while connected).
exec 3> >("${CARGO_TARGET_DIR:-target}/debug/examples/fake_daemon" --demo > "$out/fake_daemon.out")
for _ in $(seq 180); do [ "$(wc -l < "$out/fake_daemon.out")" -ge 2 ] && break; sleep 1; done
[ "$(wc -l < "$out/fake_daemon.out")" -ge 2 ] || { echo "the fake daemons did not start" >&2; exit 1; }
runtime=$(xcrun simctl list runtimes iOS -j | python3 -I -c \
  'import json,sys; print([r["identifier"] for r in json.load(sys.stdin)["runtimes"] if r["isAvailable"]][-1])')
iphone=$(xcrun simctl list devicetypes iPhone -j | python3 -I -c \
  'import json,sys; print([t["identifier"] for t in json.load(sys.stdin)["devicetypes"] if "Pro" in t["name"]][-1])')
sim=$(xcrun simctl create herder-launch-profile "$iphone" "$runtime")
cleanup_sim() { xcrun simctl delete "$sim" 2>/dev/null || true; }
trap 'cleanup_sim; rm -rf "${key_dir:-}"' EXIT
xcrun simctl boot "$sim"
xcrun simctl install "$sim" "$derived/Build/Products/Release-iphonesimulator/herder.app"
SIMCTL_CHILD_HERDER_FAKE_FLEET=$(head -1 "$out/fake_daemon.out") \
SIMCTL_CHILD_HERDER_FAKE_PASSKEY=$(sed -n 2p "$out/fake_daemon.out") \
  xcrun simctl launch "$sim" "$bundle" > /dev/null
sleep 40
xcrun simctl terminate "$sim" "$bundle"
profile="$(xcrun simctl get_app_container "$sim" "$bundle" data)/Library/Application Support/herder"
ls "$profile/cache/"*.json > /dev/null || { echo "the simulator saved no offline cache" >&2; exit 1; }

xcrun devicectl device install app --device "$device" \
  "$derived/Build/Products/Release-iphoneos/herder.app" > /dev/null
xcrun devicectl device copy to --device "$device" --domain-type appDataContainer \
  --domain-identifier "$bundle" --source "$profile" \
  --destination "Library/Application Support/herder" > /dev/null

: > "$out/launches.txt"
for i in $(seq "$launches"); do
  log=$out/launch-$i.log
  xcrun devicectl device process launch --device "$device" --terminate-existing --console \
    --environment-variables '{"HERDER_LAUNCH_REPORT": "1"}' "$bundle" > "$log" 2>&1 &
  launcher=$!
  for _ in $(seq 30); do grep -q '^herder-launch ' "$log" && break; sleep 1; done
  kill "$launcher" 2>/dev/null || true
  wait "$launcher" 2>/dev/null || true
  line=$(grep '^herder-launch ' "$log" | head -1 | tr -d '\r') ||
    { cat "$log" >&2; echo "launch $i did not reach the Board" >&2; exit 1; }
  echo "launch $i: ${line#herder-launch }"
  echo "${line#herder-launch }" >> "$out/launches.txt"
done

python3 -I - "$out/launches.txt" "$budget" <<'EOF'
import statistics, sys
runs = [dict(pair.split("=") for pair in line.split()) for line in open(sys.argv[1])]
budget = int(sys.argv[2])
steps = list(runs[0])
print("median ms:", " ".join(f"{s}={statistics.median(int(r[s]) for r in runs):.0f}" for s in steps))
# The Board is seen once its rows are drawn and the splash is gone, whichever comes last.
seen = statistics.median(max(int(r["content"]), int(r["shown"])) for r in runs)
if seen > budget:
    sys.exit(f"the Board showed after {seen:.0f} ms, over the {budget} ms budget")
print(f"the Board showed after {seen:.0f} ms, within the {budget} ms budget")
EOF
