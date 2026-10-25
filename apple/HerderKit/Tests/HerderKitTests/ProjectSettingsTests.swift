import Foundation
import Herder
@testable import HerderKit
import Testing

private func project(_ id: ProjectId, paths: [String], remote: String? = "git@github.com:acme/app.git") -> Project {
    Project(projectId: id, name: "app", paths: paths, remote: remote, defaultPermissionMode: .autoEdit,
            defaultAccount: nil, setupCommand: "make bootstrap")
}

struct ProjectMachinesTests {
    @Test func eachCloneCountsItsSessionsAndOnlyLiveOnesBlockRemoving() {
        let app = project("github.com/acme/app", paths: ["/src/app"])
        var laptop = machine("a", name: "laptop", sessions: ["01A", "01B", "01C"], projects: [app])
        laptop.sessions[0].projectId = "github.com/acme/app"
        laptop.sessions[1].projectId = "github.com/acme/app"
        laptop.sessions[1].status = .archived
        laptop.sessions[2].projectId = "github.com/acme/other"
        let server = machine("b", name: "server", sessions: [], projects: [project("github.com/acme/app", paths: ["/srv/app", "/srv/app-2"])],
                             role: .member)
        let reach = ProjectReach(projectId: "github.com/acme/app",
                                 machines: [laptop, server, machine("c", name: "mini", sessions: [])])
        #expect(reach.clones == [
            .init(hostId: "a", name: "laptop", paths: ["/src/app"], sessions: 2, live: 1, owner: true),
            .init(hostId: "b", name: "server", paths: ["/srv/app", "/srv/app-2"], sessions: 0, live: 0, owner: false),
        ])
        #expect(reach.missing.map(\.name) == ["mini"])
    }
}

@MainActor
struct ProjectCloneTests {
    /// What the Machines group's Clone does on a machine without the project: the machine
    /// clones the remote into its projects folder, and the project lists it as a clone.
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func cloningOntoAMachineWithoutTheProjectAddsItThere() async throws {
        let source = try FakeDaemon(name: "clone-source")
        let target = try FakeDaemon(name: "clone-target")
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileWriteUnknown)
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        for daemon in [source, target] {
            let machine = try await fleet.pair(daemon)
            try await fleet.client.synced(hostId: machine.hostId)
        }
        let hostId = try #require(fleet.machines.first { $0.name == "clone-target" }?.hostId)
        let projectId = try await fleet.cloneProject(source.repo, into: nil, on: hostId)
        #expect(await eventually {
            ProjectReach(projectId: projectId, machines: fleet.machines).clones.contains { $0.hostId == hostId }
        })
        let clone = try #require(ProjectReach(projectId: projectId, machines: fleet.machines).clones.first)
        #expect(clone.name == "clone-target" && !clone.paths.isEmpty)
    }
}

#if os(macOS)
import AppKit

/// Draws a project's settings page over a fleet of three machines: the project on two, one of
/// them with sessions, and missing on the third, which offers Clone; attaches it, and with
/// `HERDER_SCREENSHOT_DIR` set, also writes it there as `project-settings.png`.
@MainActor struct ProjectSettingsScreenshotTests {
    @Test func drawsAProjectsSettingsPage() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileWriteUnknown)
        }
        let id = "github.com/acme/herder"
        var work = machine("w", name: "work-mbp", sessions: ["01A", "01B", "01C"],
                           projects: [project(id, paths: ["~/Projects/herder"], remote: "git@github.com:acme/herder.git")],
                           accounts: [Account(accountId: "work", provider: "claude", label: "work", configDir: nil,
                                              email: nil, usage: [])])
        for index in work.sessions.indices { work.sessions[index].projectId = id }
        let server = machine("t", name: "trash-can-01", sessions: [],
                             projects: [project(id, paths: ["~/src/herder"], remote: "git@github.com:acme/herder.git")])
        fleet.setMachinesForTesting([work, server, machine("d", name: "draugelis", sessions: [])])
        let group = try #require(fleet.lists.projects.first { $0.projectId == id })

        let page = VStack(spacing: 0) {
            PageTrail(crumbs: ["herder", "Settings"]) { _ in }
            ProjectSettingsPage(fleet: fleet, projectId: group.id)
        }
        let host = NSHostingView(rootView: page.environment(\.colorScheme, .dark))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: 1040, height: 1100)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        // The index shows once the page has read its sections.
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        #expect(rep.pixelsWide >= 1040)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "project-settings.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("project-settings.png"))
        }
    }
}
#endif
