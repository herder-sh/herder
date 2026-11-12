#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws each step of Welcome, the first launch of a device with no machines, as the Mac's
/// window and an iPhone's screen show it: the choice, then the new path's name, machine,
/// approve and done. With `HERDER_SCREENSHOT_DIR` set, also writes them there, as
/// `welcome-<step>-<mac|iphone>.png`.
@MainActor struct WelcomeScreenshotTests {
    nonisolated static let devices = ["mac", "iphone"]

    static let own = "herder://own?host=192.168.1.20%3A7447&fp=\(String(repeating: "ab", count: 32))&code=ROS-734&name=trash-can-01"

    @Test(arguments: devices)
    func drawsTheChoice(device: String) throws {
        try shoot(WelcomeFlow(), device: device, named: "choice")
    }

    @Test(arguments: devices)
    func drawsYourName(device: String) throws {
        let flow = WelcomeFlow(name: "Tomas")
        flow.begin()
        try shoot(flow, device: device, named: "1-name")
    }

    @Test(arguments: devices)
    func drawsInstallingOnAMachine(device: String) throws {
        let flow = WelcomeFlow(name: "Tomas")
        flow.step = .machine
        try shoot(flow, device: device, named: "2-machine")
    }

    @Test(arguments: devices)
    func drawsApproving(device: String) throws {
        let flow = WelcomeFlow(name: "Tomas")
        flow.step = .machine
        flow.take(Self.own)
        try shoot(flow, device: device, named: "3-approve")
    }

    @Test(arguments: devices)
    func drawsDone(device: String) async throws {
        let flow = WelcomeFlow(name: "Tomas")
        flow.step = .machine
        flow.take(Self.own)
        await flow.approve { _, _ in "trash-can-host" }
        try shoot(flow, device: device, named: "4-done")
    }

    private func shoot(_ flow: WelcomeFlow, device: String, named step: String) throws {
        let phone = device == "iphone"
        // The Mac's window at its smallest comfortable size; an iPhone 17 Pro's screen in points.
        let (width, height) = phone ? (402.0, 874.0) : (960.0, 680.0)
        let screen = WelcomeScreen(flow: flow, existing: {}, scan: phone ? {} : nil, paste: {}, approve: {},
                                   finish: { _ in }, biometry: phone ? "Face ID" : "Touch ID")
            .environment(\.welcomeCompact, phone)
            .environment(\.welcomeDevice, phone ? "iPhone" : "Mac")
            .environment(\.exploreDemo, {})
        let view = NSHostingView(rootView: screen.environment(\.colorScheme, .dark).frame(width: width, height: height))
        view.frame = CGRect(x: 0, y: 0, width: width, height: height)
        let window = NSWindow(contentRect: view.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = view
        view.layoutSubtreeIfNeeded()
        let rep = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        let name = "welcome-\(step)-\(device).png"
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}
#endif
