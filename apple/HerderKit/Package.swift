// swift-tools-version:6.2
// The app itself, shared by the iOS and macOS targets in project.yml: models over the client
// core's Swift bindings (`Herder`, crates/herder-ffi/swift) and the SwiftUI views. Build the
// bindings first with scripts/build-ffi.sh.

import PackageDescription

let package = Package(
    name: "HerderKit",
    platforms: [.iOS(.v26), .macOS(.v26)],
    products: [
        .library(name: "HerderKit", targets: ["HerderKit"]),
    ],
    dependencies: [
        .package(name: "Herder", path: "../../crates/herder-ffi/swift"),
        .package(url: "https://github.com/migueldeicaza/SwiftTerm", from: "1.20.0"),
        .package(url: "https://github.com/swhitty/SwiftDraw", from: "0.29.0"),
        // Crashes, hangs, slow starts and traces, only while the person shares diagnostics.
        .package(url: "https://github.com/getsentry/sentry-cocoa", from: "9.36.0"),
        .package(url: "https://github.com/sindresorhus/KeyboardShortcuts", from: "3.1.0"),
    ],
    targets: [
        .target(
            name: "HerderKit",
            dependencies: [
                .product(name: "Herder", package: "Herder"),
                .product(name: "SwiftTerm", package: "SwiftTerm"),
                // Draws SVG project icons, which UIImage cannot read.
                .product(name: "SwiftDraw", package: "SwiftDraw"),
                // Built from source, as Sentry recommends: the prebuilt `Sentry` product is a
                // static xcframework that Xcode embeds as a stub Sentry.framework, which App Store
                // Connect rejects as not properly signed. Its privacy manifest is App/'s.
                .product(name: "SentrySPM", package: "sentry-cocoa"),
                // Speak to Chat's system-wide hotkey, and its recorder in Settings.
                .product(name: "KeyboardShortcuts", package: "KeyboardShortcuts", condition: .when(platforms: [.macOS])),
            ],
            // Mermaid 11.17.2 (MIT, LICENSE alongside) renders diagrams offline;
            // demo.json is the demo fleet the empty state offers, from scripts/record-demo.sh.
            resources: [.copy("Resources/Mermaid"), .copy("Resources/demo.json")],
            // VideoPlayer's view subclasses AVKit's player view, but the SwiftUI overlay it lives
            // in does not pull AVKit in: unlinked, the app aborts the moment a video artifact shows.
            linkerSettings: [.linkedFramework("AVKit")]),
        .testTarget(name: "HerderKitTests", dependencies: ["HerderKit"]),
    ]
)
