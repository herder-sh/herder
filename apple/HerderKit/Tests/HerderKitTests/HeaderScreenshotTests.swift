#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the Mac window's headers as the app's dark appearance shows them: a project's list pane
/// beside an open session waiting for the user with a PR, that session at the window's full
/// width, and the Machines page; attaches each, and with `HERDER_SCREENSHOT_DIR` set, also
/// writes them there as `headers-pane.png`, `headers-wide.png` and `headers-machines.png`.
@MainActor @Suite(.serialized) struct HeaderScreenshotTests {
    @Test func drawsTheListPaneBesideTheSession() async throws {
        let (fleet, project, key) = try await sampleFleet()
        try await shoot(fleet, item: .project(project), session: key, width: 1100, named: "headers-pane.png")
    }

    @Test func drawsTheSessionAtFullWidth() async throws {
        let (fleet, project, key) = try await sampleFleet()
        UserDefaults.standard.set(true, forKey: "listHidden")
        defer { UserDefaults.standard.removeObject(forKey: "listHidden") }
        try await shoot(fleet, item: .project(project), session: key, width: 1440, named: "headers-wide.png")
    }

    @Test func drawsAPageHeader() async throws {
        let (fleet, _, _) = try await sampleFleet()
        try await shoot(fleet, item: .machines, session: nil, width: 1440, named: "headers-machines.png")
    }

    private func shoot(_ fleet: Fleet, item: SidebarItem, session: SessionKey?, width: CGFloat,
                       named name: String) async throws {
        let view = DesktopShell(fleet: fleet, sheet: .constant(nil), item: .constant(item), pages: .constant(PageStack()),
                                session: .constant(session), draft: .constant(nil), opened: { _ in })
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: width, height: 420)
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

    /// An owned machine with a project of many tasks, the first waiting on the user with an open
    /// PR, on a release branch.
    private func sampleFleet() async throws -> (Fleet, String, SessionKey) {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let ids = (0..<24).map { String(format: "01K%023d", $0) }
        var studio = machine("studio", name: "studio", sessions: ids, projects: [
            Project(projectId: "github.com/acme/herder", name: "herder", paths: [], defaultPermissionMode: nil,
                    defaultAccount: nil, setupCommand: nil),
        ])
        for index in studio.sessions.indices { studio.sessions[index].projectId = "github.com/acme/herder" }
        fleet.setMachinesForTesting([studio])
        for (index, id) in ids.enumerated() {
            var script = Script(id, host: "studio")
            var events = [script.event(created(task: "Task \(index)", branch: index == 0 ? "release-0.13.0" : "herder/\(index)")),
                          script.event(.titleChanged(title: index == 0 ? "Herder apps UI rethink mockups" : "Task \(index)",
                                                     source: .auto))]
            if index == 0 {
                events.append(script.event(.prLinked(pr: PullRequest(
                    number: 470, url: "https://github.com/acme/herder/pull/470", title: "Release", headBranch: "release-0.13.0",
                    state: .open, ci: .passing, review: .none, mergeable: .clean))))
                events.append(script.event(.approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i",
                                                              summary: "Run cargo test?", routedTo: .user, reason: nil)))
            }
            fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        }
        // The lists rebuild a moment after the sessions change.
        try await Task.sleep(for: .milliseconds(300))
        let project = try #require(fleet.lists.projects.first).id
        return (fleet, project, SessionKey(hostId: "studio", sessionId: ids[0]))
    }
}
#endif
