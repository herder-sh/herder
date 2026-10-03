// The Kotlin bindings: `android` packages them as an AAR, `jvm` tests them on the host.
// Both build from what `build/` holds; see crates/herder-ffi/README.md.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "herder"
include(":android", ":jvm")
