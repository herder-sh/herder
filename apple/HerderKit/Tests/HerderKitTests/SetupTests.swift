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
        let found = SetupProblem.found(machines: machines, overview: overview(machines), unclaimed: true, directory: nil)
        #expect(found.map(\.id) == ["offline.h-dr", "claim", "signed-out.h-can/ops", "herder-update", "cli-updates"])
        #expect(found.map(\.suggestion) == [false, false, false, true, true])
        #expect(found[0].title == "draugelis isn’t connected" && found[0].fix == .reconnect)
        #expect(found[2].title == "Claude · ops is signed out on trash-can-01")
        #expect(found[3].title == "herder 0.21.0 is out" && found[3].detail == "trash-can-01 runs 0.20.0.")
        #expect(found[3].fix == .updateHerder(["h-can"]) && found[3].action == "Update")
        #expect(found[4].title.hasSuffix("CLI updates") && found[4].detail.hasSuffix("on trash-can-01."))
        // Nothing in the UI says "claim".
        #expect(found.allSatisfy { !"\($0.title) \($0.detail) \($0.action)".lowercased().contains("claim") })
    }

    @Test func nothingNeedsYouWhenAllIsWell() {
        var machines = sampleFleetMachines(now: now)
        machines[1].accounts = machines[1].accounts.map { var account = $0; account.needsLogin = nil; return account }
        machines[1].providers = machines[0].providers
        let found = SetupProblem.found(machines: machines, overview: overview(machines), unclaimed: false, directory: nil)
        #expect(found.isEmpty)
        #expect(SetupProblem.status(found, machines: machines, directory: Directory.demo(now: now))
            == "Everything is set up · 3 machines · 3 devices")
    }

    @Test func aDeviceMissingFromAMachineYouOwnNeedsYou() {
        let directory = Directory.demo(now: now)
        let missing = SetupProblem.missingDevices(directory)
        // The iPhone is not on draugelis; this Mac is on every one, and a terminal is on its own.
        #expect(missing.map(\.title) == ["Your iPhone isn’t on draugelis yet"])
        #expect(missing[0].fix == .addDevice)
        #expect(SetupProblem.status(missing, machines: sampleFleetMachines(now: now), directory: directory)
            == "1 needs you · 3 machines · 3 devices")
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
        #expect(SetupView.you(directory) == "Owner of every machine · passkey in iCloud Keychain")
    }

    @Test func machinesAndVaultsShareOneTable() throws {
        let machines = sampleFleetMachines(now: now)
        let vault = try #require(VaultStats(machine: machines[3], sessions: [:], now: now))
        // work-mbp has not reached the vault for six hours; trash-can-01 is current.
        #expect(MachinesView.backups([vault]) == ["h-mbp": "6 h behind", "h-can": "Up to date"])
        let line = MachineLine.vault(.paired(vault))
        #expect(line.id == "paired.h-vault" && line.running == -1 && line.role == "owner")
        #expect(MachinesView.status(.connected) == "Connected")
    }
}
