#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws Setup as the app's dark appearance shows it: the Mac's page on `sampleFleetMachines`
/// with the mockup's people, with what needs you at its top; a person's sheet; and Needs you's
/// cards at the iPhone's width, their fixes under them.
/// Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them there as
/// `setup-<screen>-<width>.png`.
@MainActor @Suite(.serialized) struct SetupScreenshotTests {
    private let now = Timestamp.date("2026-10-29T12:00:00Z")!

    /// Each machine's people: you own all four, Sam is a member of the three that run sessions,
    /// and Kim owns draugelis; your iPhone is not on draugelis yet.
    nonisolated static let people: [HostId: [PersonInfo]] = {
        let mac = FleetPeople.thisClient + "/0.21.0"
        let me = { (devices: [UserDevice]) in person("p-tomas", "Tomas", .owner, devices: devices) }
        return [
            "h-mbp": [me([PeopleTests.device("m1", mac, online: true),
                          PeopleTests.device("i1", "herder-ios/0.21.0", lastSeen: "2026-10-29T10:00:00Z")]),
                      person("p-sam", "Sam", .member), person("p-kim", "Kim", .member)],
            "h-can": [me([PeopleTests.device("m2", mac, online: true),
                          PeopleTests.device("i2", "herder-ios/0.21.0", lastSeen: "2026-10-29T10:00:00Z")]),
                      person("p-sam", "Sam", .member),
                      person("p-kim", "Kim", .member, devices: [PeopleTests.device("k1", "herder-ios/0.21.0", online: true)])],
            "h-dr": [me([PeopleTests.device("m3", mac, online: true)]), person("p-sam", "Sam", .member),
                     person("p-kim", "Kim", .owner, devices: [PeopleTests.device("k2", "herder-ios/0.21.0", online: true),
                                                              PeopleTests.device("k3", "herder-tui/0.21.0", online: true)])],
            "h-vault": [me([])],
        ]
    }()

    @Test func drawsTheMacPage() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        var machines = sampleFleetMachines(now: now)
        for index in machines.indices {
            machines[index].personId = "p-tomas"
            machines[index].role = .owner
        }
        machines[0].herderVersion = "0.21.0"
        machines[1].herderVersion = "0.20.0"
        fleet.setMachinesForTesting(machines)
        let people = FleetPeople { hostId, _ in .people(people: Self.people[hostId] ?? []) }
        await people.reload(machines)
        let switcher = Switcher(fleet: fleet, item: .constant(.setup), session: .constant(nil), order: ProjectOrder(sort: .manual, stored: ""))
        let page = SetupPane(fleet: fleet, people: people, sheet: .constant(nil), switcher: switcher)
        try await shoot(page, size: CGSize(width: 1180, height: 1500), named: "setup-mac.png")
    }

    @Test func drawsAPersonsSheet() async throws {
        let directory = Directory.demo(now: now)
        let sheet = PersonSheet(directory: directory, person: directory.teammates[1], setRole: { _, _ in },
                                signOut: { _ in }, itsMe: {})
        try await shoot(sheet, size: CGSize(width: 480, height: 820), named: "setup-person-mac.png")
    }

    @Test func drawsNeedsYouAtTheIPhonesWidth() async throws {
        let directory = Directory.demo(now: now)
        var machines = sampleFleetMachines(now: now)
        machines[0].herderVersion = "0.21.0"
        machines[1].herderVersion = "0.20.0"
        let overview = ProvidersOverview(accounts: ProviderAccounts(machines: machines, now: now),
                                         versions: ProviderVersions(machines: machines), installs: [:])
        let problems = SetupProblem.found(machines: machines, overview: overview, unclaimed: false, directory: directory)
        let view = NeedsYouSection(problems: problems) { _ in }.padding(16)
        try await shoot(view, size: CGSize(width: 390, height: 900), named: "setup-needs-you-iphone.png")
    }

    private func shoot(_ content: some View, size: CGSize, named name: String) async throws {
        let view = content
            .frame(width: size.width, height: size.height, alignment: .topLeading)
            .layer(.canvas)
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
