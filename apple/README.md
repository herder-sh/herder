# apple

The herder app for iOS and macOS: one SwiftUI codebase on the client core's Swift bindings
(`crates/herder-ffi`). Its profile, with the paired machines and the offline cache, lives in
the app's Application Support directory.

- `HerderKit/`: the app itself, a Swift package with the models and the views.
- `App/`: the thin iOS and macOS app targets over it.
- `UITests/`: the iOS UI tests.
- `project.yml`: the Xcode project, generated with [XcodeGen](https://github.com/yonaskolb/XcodeGen).

## Build

Needs Xcode with its Metal toolchain (`xcodebuild -downloadComponent MetalToolchain`, for SwiftTerm), rustup and XcodeGen (`brew install xcodegen`).

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin
apple/scripts/build-ffi.sh          # bindings and HerderFFI.xcframework, into crates/herder-ffi/swift/build
xcodegen generate --spec apple/project.yml
open apple/herder.xcodeproj         # schemes herder-iOS and herder-macOS
```

Run `build-ffi.sh` again whenever the Rust side changes. The xcframework has arm64 slices
only, so the apps run on Apple silicon Macs and simulators.

## Release

`apple/scripts/build-mac-release.sh` (after `build-ffi.sh`) builds the macOS app in Release,
ad-hoc signs it and zips it into `dist/herder-app-<version>-macos-arm64.zip`, with its
`.sha256`. The `apple` workflow uploads that zip as the `herder-app-macos-arm64` artifact, and
the `release` workflow attaches it to the GitHub Release of each `v*` tag, next to the CLI.

The app is not signed with a Developer ID nor notarized yet, so Gatekeeper blocks it on first
open. To install it on an Apple silicon Mac:

1. Download the zip from the release (or the workflow run's artifacts) and unzip it.
2. Move `herder.app` to `/Applications`.
3. Clear the quarantine flag: `xattr -dr com.apple.quarantine /Applications/herder.app`.
   Or right-click the app, choose **Open**, then **Open** again (on recent macOS: try to open
   it once, then **System Settings › Privacy & Security › Open Anyway**).

There is no iOS build to install yet: TestFlight needs an Apple Developer account.

## Test

The HerderKit tests pair with the fake daemon `build-ffi.sh` builds:

```sh
HERDER_FAKE_DAEMON=$PWD/target/debug/examples/fake_daemon swift test --package-path apple/HerderKit
```

The iOS UI test pairs the app on a simulator with a running fake daemon:

```sh
cargo run -p herder-ffi --example fake_daemon   # prints the pairing link first
TEST_RUNNER_HERDER_PAIR_LINK='<link>' xcodebuild test -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-iOS -destination 'platform=iOS Simulator,name=iPhone 18 Pro'
```

A pairing code works once, so start a new fake daemon for each run.

To try the app against it, run `cargo run -p herder-ffi --example fake_daemon` and paste the
link it prints into **Add Machine**. For a real machine, run `herder pair` on it.

The `apple` workflow runs the tests and builds both apps on every change to `apple/` or to what
it builds on.
