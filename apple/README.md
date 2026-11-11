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
only, so the apps run on Apple silicon Macs and simulators. Working on the Mac app or HerderKit
alone, `apple/scripts/build-ffi.sh aarch64-apple-darwin` builds just the Mac slice, which is
much faster.

## Release

`apple/scripts/build-mac-release.sh` (after `build-ffi.sh`) builds the macOS app in Release
and zips it into `dist/herder-app-<version>-macos-arm64.zip`, with its `.sha256`. The `release`
workflow attaches it to the GitHub Release of each `v*` tag, next to the CLI. There the script
has the App Store Connect secrets below: Xcode signs the app with the team's Developer ID
through cloud-managed certificates, with a Developer ID provisioning profile that grants it the
herder.sh associated domains passkeys need, and the script notarizes and staples it. It fails
the build unless the app carries its application identifier and associated domains and
Gatekeeper accepts it. Download the zip, unzip it and move `herder.app` to `/Applications`.

Without those secrets, as in the `apple` workflow on each push to main (its
`herder-app-macos-arm64` artifact), the script signs the app ad hoc, without associated
domains. Such a copy opens only through **System Settings › Privacy & Security › Open Anyway**
and cannot use your passkey. On a Mac the team's development profile lists,
`apple/scripts/install-mac.sh` builds and installs a team-signed copy instead.

The iOS app goes out through TestFlight. The `release` workflow's `testflight` job runs
`apple/scripts/upload-testflight.sh` on each `v*` tag, and on workflow_dispatch: it archives the
app in Release with the herder version and the run number as its build, and uploads it to App
Store Connect. Xcode signs it, like the Mac app, with cloud-managed certificates through an App
Store Connect API key, so the repo keeps no certificate or profile, only these secrets:

- `APPLE_TEAM_ID`: the Apple Developer team the app belongs to.
- `ASC_KEY_ID`, `ASC_ISSUER_ID`: the App Store Connect API key (Users and Access › Integrations),
  with the Admin role, which cloud signing needs. It also notarizes the Mac app.
- `ASC_KEY`: the contents of the key's `.p8` file.

App Store Connect needs an app record for `sh.herder.Herder` before the first upload. Once a
build has processed, the testers in the app's internal TestFlight group get it in the TestFlight
app.

## Test

The HerderKit tests pair with the fake daemon `build-ffi.sh` builds:

```sh
HERDER_FAKE_DAEMON=$PWD/target/debug/examples/fake_daemon swift test --package-path apple/HerderKit
```

The iOS UI tests sign the app in on a simulator to two running fake daemons, from the link a
device signed in to both shares and with the passkey of their owner, which the fake daemons
print, then open the session waiting on an approval on the first from its card on Home:

```sh
cargo run -p herder-ffi --example fake_daemon -- --share   # the link and passkey first, the waiting session last
cargo run -p herder-ffi --example fake_daemon -- --vault   # a vault that keeps secrets
cargo run -p herder-ffi --example fake_daemon -- --own     # a machine to approve as its owner
TEST_RUNNER_HERDER_PAIR_LINK='<link>' TEST_RUNNER_HERDER_PASSKEY='<passkey>' \
TEST_RUNNER_HERDER_WAITING_SESSION='<session id>' \
TEST_RUNNER_HERDER_VAULT_LINK='<vault link>' TEST_RUNNER_HERDER_OWN_LINK='<own link>' xcodebuild test -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-iOS -destination 'platform=iOS Simulator,name=iPhone 18 Pro'
```

Erase the app from the simulator (`xcrun simctl uninstall booted sh.herder.Herder`) so it
starts with no machines.

To try the app against it, launch the app with `HERDER_FAKE_FLEET` set to the link
`fake_daemon` prints and `HERDER_FAKE_PASSKEY` to the passkey after it: it signs in to those
machines at launch. For a real machine, run `herder claim` on it: on iOS, scan the QR code it
prints with **Scan QR Code**; on the Mac, paste the link (or all of its output), and approve
with your passkey.

The `apple` workflow runs the tests and builds both apps on every change to `apple/` or to what
it builds on.

## Profile

When the Mac app feels slow, measure it under a load that repeats:

```sh
apple/scripts/profile-mac.sh            # the streaming session; `board` for the Board
apple/scripts/profile-mac.sh board 20   # and how many seconds to record
apple/scripts/profile-mac.sh switch     # back and forth between it and a finished session
```

It builds a release copy of the app with its own bundle id and profile, so the installed app
keeps running with its machines, and signs it in to `fake_daemon --busy`: the demo fleet plus a
session with a long transcript whose turn keeps streaming. The `herder-macOS-Profile` UI test
opens the scene and holds it while Instruments records it with the SwiftUI template. Then the
script prints the hitches, how long the main thread was busy, the SwiftUI updates that took
longest and herder's heaviest frames, and keeps the trace in `target/profile/` to open in
Instruments.

Read the main thread's busy time first: hitches count every late frame on the machine, so
other apps loading the window server inflate them. Keep away from the keyboard and mouse
until the recording starts, as the test drives them.

## A blank transcript

The Mac and iOS apps log the transcript's scroll state: content height, offset, the view's
rect, insets, whether it is at the end, and the ids of the first and last blocks in view. They
log numbers and block ids only, never the transcript's text. `blank` lines (no block in view
though the session has blocks) and `outside-content` lines (the view scrolled off its content)
are errors. When a transcript goes blank, leave it blank and read the last ten minutes:

```sh
/usr/bin/log show --last 10m --info --style compact \
  --predicate 'subsystem == "sh.herder.Herder" AND category == "transcript"' > transcript.log
```

Or watch it live before it happens:

```sh
/usr/bin/log stream --level info --style compact \
  --predicate 'subsystem == "sh.herder.Herder" AND category == "transcript"'
```

## Scrolling smoothness

Each long screen has to scroll with a hitch time ratio under 5 ms/s, Apple's "good":

```sh
apple/scripts/perf.sh                 # this Mac
apple/scripts/perf.sh mac iphone      # and the iPhone whose UDID is in HERDER_PERF_DEVICE
```

The `herder-macOS-Perf` and `herder-iOS-Perf` UI tests (apple/PerfTests) open each screen on
`fake_daemon --busy` and fling it to its end and back five times while XCTest measures the
scroll's hitches. The script builds both apps for Release, runs the tests, writes the table to
`target/perf/perf.md` and fails when a screen is over the budget. The iPhone build is
`sh.herder.Herder.Perf`, beside the installed app; signing it needs the App Store Connect key
(`herder secret run apple-herder/ASC_KEY_ID apple-herder/ASC_ISSUER_ID
apple-herder/ASC_PRIVATE_KEY -- apple/scripts/perf.sh iphone`). The `apple` workflow's `perf`
job runs the Mac's on every change, with a tolerance for the runner.

Measure on a quiet machine: other apps' load, and other UI tests taking the screen, show up as
hitches.
