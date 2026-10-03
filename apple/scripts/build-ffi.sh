#!/usr/bin/env bash
# Builds what the apps link, as the `ffi` workflow does: the Swift bindings of the client core
# and HerderFFI.xcframework (iOS device, iOS simulator, macOS; arm64), into
# crates/herder-ffi/swift/build. Also builds the fake daemon the HerderKit tests pair with.
# Needs Xcode and the Rust targets below (`rustup target add ...`).
set -euo pipefail

cd "$(dirname "$0")/../.."
swift=crates/herder-ffi/swift
target_dir=${CARGO_TARGET_DIR:-target}
targets=(aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin)
# Match the bindings package's platforms, for the C and assembly that ring builds.
export MACOSX_DEPLOYMENT_TARGET=13.0 IPHONEOS_DEPLOYMENT_TARGET=16.0

cargo build -p herder-ffi --features bindgen --lib --bin uniffi-bindgen --example fake_daemon

generated=$(mktemp -d)
"$target_dir/debug/uniffi-bindgen" generate --library "$target_dir/debug/libherder_ffi.dylib" \
  --language swift --no-format --out-dir "$generated"
rm -rf "$swift/build"
mkdir -p "$swift/build/Sources/Herder" "$swift/build/headers"
mv "$generated/Herder.swift" "$swift/build/Sources/Herder/"
mv "$generated/HerderFFI.h" "$swift/build/headers/"
mv "$generated/HerderFFI.modulemap" "$swift/build/headers/module.modulemap"

args=()
for target in "${targets[@]}"; do
  cargo rustc -p herder-ffi --lib --release --target "$target" --crate-type staticlib
  args+=(-library "$target_dir/$target/release/libherder_ffi.a" -headers "$swift/build/headers")
done
xcodebuild -create-xcframework "${args[@]}" -output "$swift/build/HerderFFI.xcframework"
