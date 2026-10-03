# herder-ffi

UniFFI bindings of `herder-client-core` for the Swift and Kotlin apps. The API is
client-core's (`crates/herder-client-core/API.md`), with three additions described in
`src/lib.rs`: the client owns its tokio runtime, errors are `HerderError` (Kotlin:
`HerderException`), and pairing links parse and format with `parsePairingUri` and
`pairingUriToString`. Protocol ids, providers and timestamps are strings (timestamps RFC 3339),
terminal bytes are byte arrays and a tool call's input is JSON text.

The `ffi` workflow builds both packages and runs their samples; this is what it does.

## The fake daemon

`cargo run -p herder-ffi --example fake_daemon` starts a daemon whose one account (`fake`)
runs on the fake adapter, replaying `fixtures/hello.jsonl`. It prints a pairing link, the path
of a git repository to create a session on, and the account, one per line, and runs until its
stdin closes.

## Kotlin: an AAR, and a test on the host JVM

```sh
cargo build -p herder-ffi --features bindgen --lib --bin uniffi-bindgen --example fake_daemon
target/debug/uniffi-bindgen generate --library target/debug/libherder_ffi.so \
  --language kotlin --no-format --out-dir crates/herder-ffi/kotlin/build/generated/uniffi
cargo ndk -t arm64-v8a -t x86_64 -o crates/herder-ffi/kotlin/build/jniLibs \
  build -p herder-ffi --release
gradle -p crates/herder-ffi/kotlin :jvm:test :android:assembleRelease
```

`:jvm:test` loads `target/debug/libherder_ffi.so` and drives the fake daemon. The AAR lands in
`kotlin/android/build/outputs/aar/`; apps add JNA (`net.java.dev.jna:jna:5.15.0@aar`) and
kotlinx-coroutines next to it.

## Swift: an xcframework, and a sample program (macOS)

```sh
cargo build -p herder-ffi --features bindgen --lib --bin uniffi-bindgen --example fake_daemon
target/debug/uniffi-bindgen generate --library target/debug/libherder_ffi.dylib \
  --language swift --no-format --out-dir /tmp/swift
# Herder.swift -> swift/build/Sources/Herder/
# HerderFFI.h and HerderFFI.modulemap (as module.modulemap) -> swift/build/headers/
for target in aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin; do
  cargo rustc -p herder-ffi --lib --release --target $target --crate-type staticlib
done
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libherder_ffi.a -headers crates/herder-ffi/swift/build/headers \
  -library target/aarch64-apple-ios-sim/release/libherder_ffi.a -headers crates/herder-ffi/swift/build/headers \
  -library target/aarch64-apple-darwin/release/libherder_ffi.a -headers crates/herder-ffi/swift/build/headers \
  -output crates/herder-ffi/swift/build/HerderFFI.xcframework
swift run --package-path crates/herder-ffi/swift herder-sample "$PWD/target/debug/examples/fake_daemon"
```

The Swift package in `swift/` exposes the bindings as the `Herder` library.
