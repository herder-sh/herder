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
        .package(url: "https://github.com/migueldeicaza/SwiftTerm", from: "1.20.0"),
    ],
    targets: [
        .target(
            name: "HerderKit",
            dependencies: [
                .product(name: "Herder", package: "Herder"),
                .product(name: "SwiftTerm", package: "SwiftTerm"),
            ],
            // Mermaid 11.17.2 (MIT, LICENSE alongside) renders diagrams offline.
            resources: [.copy("Resources/Mermaid")]),
        .testTarget(name: "HerderKitTests", dependencies: ["HerderKit"]),
    ]
)
