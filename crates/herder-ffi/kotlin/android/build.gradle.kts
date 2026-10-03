plugins {
    id("com.android.library")
    kotlin("android")
}

android {
    namespace = "sh.herder.ffi"
    compileSdk = 35
    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
    }
    sourceSets["main"].java.srcDir(rootProject.extra["uniffiSources"]!!)
    sourceSets["main"].jniLibs.srcDir(rootProject.extra["jniLibs"]!!)
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    implementation("net.java.dev.jna:jna:5.15.0@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
