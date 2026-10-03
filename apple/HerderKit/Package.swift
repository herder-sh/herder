// swift-tools-version:6.0
// The app itself, shared by the iOS and macOS targets in project.yml: models over the client
// core's Swift bindings (`Herder`, crates/herder-ffi/swift) and the SwiftUI views. Build the
// bindings first with scripts/build-ffi.sh.

import PackageDescription

let package = Package(
    name: "HerderKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "HerderKit", targets: ["HerderKit"]),
    ],
    dependencies: [
        .package(name: "Herder", path: "../../crates/herder-ffi/swift"),
    ],
    targets: [
        .target(name: "HerderKit", dependencies: [.product(name: "Herder", package: "Herder")]),
        .testTarget(name: "HerderKitTests", dependencies: ["HerderKit"]),
    ]
)
