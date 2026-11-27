import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct SetupTests {
    private let now = Timestamp.date("2026-10-29T12:00:00Z")!

    /// `sampleFleetMachines` with draugelis offline, and work-mbp a herder ahead of trash-can-01.
    private func machines() -> [Machine] {
        var machines = sampleFleetMachines(now: now)
        machines[0].herderVersion = "0.21.0"
        machines[1].herderVersion = "0.20.0"
        machines[2].connection = .disconnected(error: "connection refused")
        return machines
    }

    private func overview(_ machines: [Machine]) -> ProvidersOverview {
        ProvidersOverview(accounts: ProviderAccounts(machines: machines, now: now),
                          versions: ProviderVersions(machines: machines), installs: [:])
    }

    @Test func problemsComeFirstEachWithItsFixThenSuggestions() {
        let machines = machines()
        let found = HubProblem.found(machines: machines, overview: overview(machines), unclaimed: true, directory: nil)
        #expect(found.map(\.id) == ["claim", "signed-out.h-can/ops", "herder-update", "cli-updates"])
        #expect(found.map(\.suggestion) == [false, false, true, true])
        #expect(found[1].title == "Claude · ops is signed out on trash-can-01")
        #expect(found[2].title == "herder 0.21.0 is out" && found[2].detail == "trash-can-01 runs 0.20.0.")
        #expect(found[2].fix == .updateHerder(["h-can"]) && found[2].action == "Update")
        #expect(found[3].title == "Claude and Codex have updates" && found[3].detail == "On trash-can-01.")
        // Only the problems need you; an update never does.
        #expect(found.needingYou.map(\.id) == ["claim", "signed-out.h-can/ops"])
        #expect(found.updates.map(\.id) == ["herder-update", "cli-updates"])
        #expect(HubProblem.status(found) == "2 need you")
        // Nothing in the UI says "claim".
        #expect(found.allSatisfy { !"\($0.title) \($0.detail) \($0.action)".lowercased().contains("claim") })
    }

    /// A laptop asleep or away is expected: an offline machine never needs you.
    @Test func anOfflineMachineNeedsNothing() {
        var machines = sampleFleetMachines(now: now)
        machines[1].accounts = machines[1].accounts.map { var account = $0; account.needsLogin = nil; return account }
        machines[1].providers = machines[0].providers
        for index in machines.indices {
            machines[index].connection = .disconnected(error: "192.168.1.147:7447: no answer in 10s")
        }
        let found = HubProblem.found(machines: machines, overview: overview(machines), unclaimed: false, directory: nil)
        #expect(found.isEmpty)
    }

    @Test func nothingNeedsYouWhenAllIsWell() {
        var machines = sampleFleetMachines(now: now)
        machines[1].accounts = machines[1].accounts.map { var account = $0; account.needsLogin = nil; return account }
        machines[1].providers = machines[0].providers
        let found = HubProblem.found(machines: machines, overview: overview(machines), unclaimed: false, directory: nil)
        #expect(found.isEmpty)
        #expect(HubProblem.status(found) == "Everything is set up")
    }

    /// An update stops nothing, so it is not in the badge's count or the header's line.
    @Test func anUpdateNeverNeedsYou() {
        var machines = sampleFleetMachines(now: now)
        machines[1].accounts = machines[1].accounts.map { var account = $0; account.needsLogin = nil; return account }
        machines[0].herderVersion = "0.21.0"
        machines[1].herderVersion = "0.20.0"
        let found = HubProblem.found(machines: machines, overview: overview(machines), unclaimed: false, directory: nil)
        #expect(found.map(\.id) == ["herder-update", "cli-updates"])
        #expect(found.needingYou.isEmpty)
        #expect(HubProblem.status(found) == "Everything is set up")
    }

    @Test func aDeviceMissingFromAMachineYouOwnNeedsYou() {
        let directory = Directory.demo(now: now)
        let missing = HubProblem.missingDevices(directory)
        // The iPhone is not on draugelis; this Mac is on every one, and a terminal is on its own.
        #expect(missing.map(\.title) == ["Your iPhone isn’t on draugelis yet"])
        #expect(missing[0].fix == .addDevice)
        #expect(HubProblem.status(missing) == "1 needs you")
    }

    @Test func aTeammatesDevicesAreTheirsAndNeverThisOne() async throws {
        let people = FleetPeople { hostId, _ in .people(people: FleetPeopleTests.people[hostId] ?? []) }
        let machines = [machine("devbox", name: "devbox", sessions: [], personId: "p-tomas"),
                        machine("laptop", name: "laptop", sessions: [], role: .member, personId: "p-tomas")]
        await people.reload(machines)
        let directory = try #require(people.directory(machines, localNames: []))
        let sam = try #require(directory.teammates.first { $0.name == "Sam" })
        // Sam's Mac runs this app's client too, but only yours can be this device.
        #expect(directory.devices(of: sam).map(\.title) == ["Mac"])
        #expect(directory.devices(of: sam)[0].paired == ["devbox": ["d2"]])
        #expect(directory.devices(of: sam)[0].reach == "On 1 machine")
        #expect(directory.devices(of: directory.me).map(\.title) == ["This Mac", "iPhone"])
    }

    @Test func aPersonsSheetSaysTheirRoleAndDevices() {
        let directory = Directory.demo(now: now)
        #expect(PersonSheet.summary(directory, directory.teammates[1]) == "Different per machine · 2 devices")
        #expect(PersonSheet.summary(directory, directory.teammates[0]) == "Member on all your machines")
    }

    @Test func aDeviceSaysHowManyMachinesItIsOn() {
        let devices = Directory.demo(now: now).devices
        #expect(devices.map(DevicesSection.summary) == ["On 3 machines", "On 2 machines", "Signed in with herder login"])
    }
}
