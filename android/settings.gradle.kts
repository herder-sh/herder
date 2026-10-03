// The Compose app (`app`) and the AAR it links (`ffi`): herder-ffi's Kotlin bindings and its
// Android libraries, which `ffi` builds with cargo. See README.md.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode = RepositoriesMode.FAIL_ON_PROJECT_REPOS
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "herder-android"
include(":app", ":ffi")
