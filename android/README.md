# android

The Jetpack Compose app, built on `herder-client-core` through the Kotlin bindings of
`crates/herder-ffi`. It opens its profile in app-private storage (`files/herder`) and suspends
the client when the app goes to the background and wakes it when it returns.

It shows what the TUI's lists show, live: the paired machines with their connection state and a
vault's hosts online or offline; and the sessions of one machine or host, or of all, grouped by
project or by machine, each with its status, its task tree, how many of its tasks need you and
its PRs. On a tablet the machines and the sessions sit side by side; on a phone the sessions are
a page of their own.

Add a machine from **Add a machine**: run `herder pair` on the host, then scan the QR code it
prints (CameraX + ML Kit) or paste the `herder://pair` link — or all of `herder pair`'s
output. The fingerprint is shown to confirm before the client pairs. Opening a
`herder://pair?…` link lands on that confirm step.

A tap opens a session (docs/tui-design.md §2.1, §5): its transcript streams, each tool call one
row that expands on a tap; an approval or a question replaces the composer with a card, answered
with large Allow / Deny buttons or a swipe (right allows, left denies), a choice or typed text.
The composer sends prompts (queued while a turn runs), stops the turn, and switches the
session's account (another provider's replays the transcript), model and permission mode.
Photos, the camera and a paste attach images; thumbnails show on the user's turn and open
full size.
Its pull requests sit over the transcript (number, title, branch, state, CI, review,
mergeable); a tap opens the PR in the browser, and the session menu links another. The
machines list's Pull requests screen lists every session's the same way. Archived and moved
sessions, and a vault's, are read-only.

- `ffi` is the AAR the app links: the Gradle build runs cargo to build the host library,
  `uniffi-bindgen` to generate the bindings from it, and `cargo ndk` to build the Android
  libraries (arm64-v8a, x86_64).
- `app` is the app.

To build it you need a JDK 21, the Android SDK (`ANDROID_HOME`), an NDK (`ANDROID_NDK_HOME`),
Gradle 9.6 or later, cargo-ndk and the Android Rust targets:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --locked
gradle -p android :app:testDebugUnitTest :app:assembleDebug
```

The APK lands in `app/build/outputs/apk/debug/`, and the tests' screenshots (Roborazzi; phone
and tablet, light and dark) in `app/build/outputs/roborazzi/`. The `android` workflow runs the
same build and uploads both.
