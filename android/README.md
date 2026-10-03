# android

The Jetpack Compose app, built on `herder-client-core` through the Kotlin bindings of
`crates/herder-ffi`. It opens its profile in app-private storage (`files/herder`), shows the
paired machines with their connection state, and suspends the client when the app goes to the
background and wakes it when it returns.

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

The APK lands in `app/build/outputs/apk/debug/`. The `android` workflow runs the same build
and uploads it.
