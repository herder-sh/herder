plugins {
    kotlin("jvm")
}

kotlin {
    jvmToolchain(17)
    sourceSets["main"].kotlin.srcDir(rootProject.extra["uniffiSources"]!!)
}

dependencies {
    implementation("net.java.dev.jna:jna:5.15.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
    testImplementation(kotlin("test-junit"))
}

// The host's libherder_ffi and the fake daemon, from a debug build of the workspace.
val target = rootDir.resolve("../../../target/debug")

tasks.test {
    systemProperty("jna.library.path", target.path)
    systemProperty("herder.fakeDaemon", target.resolve("examples/fake_daemon").path)
    testLogging {
        events("passed", "failed")
        showStandardStreams = true
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
    }
}
