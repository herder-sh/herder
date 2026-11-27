#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws Hub as the app's dark appearance shows it: the Mac's page on `sampleFleetMachines`
/// with the mockup's people, what needs you at its top and the hub with its machines under it;
/// the same page at the iPhone's width, and with no hub; a person's sheet; and Needs you's
/// cards at the iPhone's width, their fixes under them.
/// Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them there as
/// `hub-<screen>.png`.
@MainActor @Suite(.serialized) struct HubScreenshotTests {
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
            "h-hub": [me([])],
        ]
    }()

    /// `sampleFleetMachines` as yours, with or without their hub: draugelis is asleep,
    /// trash-can-01 runs an older herder, and the hub has a public address.
    private func setup(hub: Bool = true) async throws -> (Fleet, FleetPeople) {
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
        machines[2].connection = .disconnected(error: "no answer in 10s")
        machines[3].addresses = ["wss://hub.example.com"]
        if !hub { machines.removeLast() }
        fleet.setMachinesForTesting(machines)
        let people = FleetPeople { hostId, _ in .people(people: Self.people[hostId] ?? []) }
        await people.reload(machines)
        return (fleet, people)
    }

    @Test func drawsTheMacPage() async throws {
        let (fleet, people) = try await setup()
        let switcher = Switcher(fleet: fleet, item: .constant(.hub), session: .constant(nil), order: ProjectOrder(sort: .manual, stored: ""))
        let page = HubPane(fleet: fleet, people: people, sheet: .constant(nil), switcher: switcher)
        try await shoot(page, size: CGSize(width: 1180, height: 1300), named: "hub-mac.png")
    }

    @Test func drawsTheIPhonePage() async throws {
        let (fleet, people) = try await setup()
        let page = HubView(fleet: fleet, people: people, sheet: .constant(nil), showsSections: true)
        try await shoot(page, size: CGSize(width: 390, height: 1500), named: "hub-iphone.png")
    }

    @Test func drawsTheIPhonePageWithNoHub() async throws {
        let (fleet, people) = try await setup(hub: false)
        let page = HubView(fleet: fleet, people: people, sheet: .constant(nil), showsSections: true)
        try await shoot(page, size: CGSize(width: 390, height: 1300), named: "hub-no-hub-iphone.png")
    }

    @Test func drawsAPersonsSheet() async throws {
        let directory = Directory.demo(now: now)
        let sheet = PersonSheet(directory: directory, person: directory.teammates[1], setRole: { _, _ in },
                                signOut: { _ in }, itsMe: {})
        try await shoot(sheet, size: CGSize(width: 480, height: 820), named: "hub-person-mac.png")
    }

    @Test func drawsNeedsYouAtTheIPhonesWidth() async throws {
        let directory = Directory.demo(now: now)
        var machines = sampleFleetMachines(now: now)
        machines[0].herderVersion = "0.21.0"
        machines[1].herderVersion = "0.20.0"
        let overview = ProvidersOverview(accounts: ProviderAccounts(machines: machines, now: now),
                                         versions: ProviderVersions(machines: machines), installs: [:])
        let problems = HubProblem.found(machines: machines, overview: overview, unclaimed: false, directory: directory)
        let view = NeedsYouSection(problems: problems.needingYou) { _ in }.padding(16)
        try await shoot(view, size: CGSize(width: 390, height: 900), named: "hub-needs-you-iphone.png")
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
