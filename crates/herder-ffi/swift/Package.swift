// swift-tools-version:5.9
// The Swift bindings: `HerderFFI` is the xcframework of the Rust library, `Herder` the
// generated Swift API over it, and `herder-sample` a program that drives a fake daemon.
// Both build from what `build/` holds; see crates/herder-ffi/README.md.

import PackageDescription

let package = Package(
    name: "Herder",
    platforms: [.macOS(.v13), .iOS(.v16)],
    products: [
        .library(name: "Herder", targets: ["Herder"]),
    ],
    targets: [
        .binaryTarget(name: "HerderFFI", path: "build/HerderFFI.xcframework"),
        .target(name: "Herder", dependencies: ["HerderFFI"], path: "build/Sources/Herder"),
        .executableTarget(name: "herder-sample", dependencies: ["Herder"], path: "Sample"),
    ]
)
