#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the stalled-work line where it shows: the Mac window with the sidebar open and with
/// the rail, while devbox is offline with a session running and one waiting for you on it; the
/// same window with every machine connected, where the sidebar's foot is empty; and the Board at
/// the iPhone's width with the line at its foot. Attaches each; with `HERDER_SCREENSHOT_DIR` set,
/// also writes them there.
@MainActor @Suite(.serialized) struct StalledWorkScreenshotTests {
    @Test func drawsTheSidebarsLine() async throws {
        let fleet = try await sampleFleet(devboxOffline: true)
        try await shoot(window(fleet), collapsed: false, size: CGSize(width: 1100, height: 640), named: "stalled-work-mac.png")
    }

    @Test func drawsTheRailsMark() async throws {
        let fleet = try await sampleFleet(devboxOffline: true)
        try await shoot(window(fleet), collapsed: true, size: CGSize(width: 1100, height: 640), named: "stalled-work-rail-mac.png")
    }

    @Test func drawsNothingWhileAllIsWell() async throws {
        let fleet = try await sampleFleet(devboxOffline: false)
        try await shoot(window(fleet), collapsed: false, size: CGSize(width: 1100, height: 640), named: "stalled-work-none-mac.png")
    }

    @Test func drawsTheBoardsLineAtTheIPhonesWidth() async throws {
        let fleet = try await sampleFleet(devboxOffline: true)
        let board = BoardView(fleet: fleet, query: "")
            .safeAreaInset(edge: .bottom, spacing: 0) { BoardStalledWork(fleet: fleet) { _ in } }
            .background(.ground)
        try await shoot(board, collapsed: true, size: CGSize(width: 390, height: 640), named: "stalled-work-iphone.png")
    }

    /// While all is well the Board's foot takes no room: padding and a background around the
    /// empty line once drew a strip of the page's ground through the iPhone's tab bar.
    @Test func takesNoRoomOnTheBoardWhileAllIsWell() async throws {
        let fleet = try await sampleFleet(devboxOffline: false)
        let host = NSHostingView(rootView: BoardStalledWork(fleet: fleet) { _ in })
        #expect(host.fittingSize == .zero)
        let stalled = NSHostingView(rootView: BoardStalledWork(fleet: try await sampleFleet(devboxOffline: true)) { _ in })
        #expect(stalled.fittingSize.height > 0)
    }

    private func window(_ fleet: Fleet) -> some View {
        DesktopShell(fleet: fleet, sheet: .constant(nil), item: .constant(.board), pages: .constant(PageStack()),
                     session: .constant(nil), draft: .constant(nil), people: FleetPeople(fleet: fleet), opened: { _ in },
                     perform: { _ in })
    }

    /// studio, connected, with a task done; devbox with a session running and one waiting on an
    /// approval, offline when `devboxOffline`.
    private func sampleFleet(devboxOffline: Bool) async throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let api = "github.com/acme/api"
        let project = Project(projectId: api, name: "api", paths: [], defaultPermissionMode: nil, defaultAccount: nil,
                              setupCommand: nil)
        // Host, session, title, and what it was doing when last heard from.
        let sessions: [(HostId, String, String, EventBody?)] = [
            ("studio", "01K00000000000000000000001", "Tidy the README badges", nil),
            ("devbox", "01K00000000000000000000002", "Run checkout e2e in more browsers",
             .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "Run pnpm e2e", routedTo: .user,
                                reason: nil, secret: nil, secretRequest: nil)),
            ("devbox", "01K00000000000000000000003", "Audit webhook retries",
             .sessionStatusChanged(status: .running, retryAt: nil)),
        ]
        var studio = machine("studio", name: "studio", sessions: [sessions[0].1], projects: [project])
        var devbox = machine("devbox", name: "devbox", sessions: [sessions[1].1, sessions[2].1], projects: [project],
                             connection: devboxOffline ? .disconnected(error: "no answer in 10s") : .connected)
        for index in studio.sessions.indices { studio.sessions[index].projectId = api }
        for index in devbox.sessions.indices { devbox.sessions[index].projectId = api }
        fleet.setMachinesForTesting([studio, devbox])
        for (host, id, title, last) in sessions {
            var script = Script(id, host: host)
            var events = [script.event(created()), script.event(.titleChanged(title: title, source: .auto))]
            if let last { events.append(script.event(last)) }
            fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        }
        // The lists rebuild a moment after the sessions change.
        try await Task.sleep(for: .milliseconds(300))
        return fleet
    }

    private func shoot(_ content: some View, collapsed: Bool, size: CGSize, named name: String) async throws {
        UserDefaults.standard.set(collapsed, forKey: "sidebarCollapsed")
        defer { UserDefaults.standard.removeObject(forKey: "sidebarCollapsed") }
        let view = content
            .frame(width: size.width, height: size.height)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}
#endif
