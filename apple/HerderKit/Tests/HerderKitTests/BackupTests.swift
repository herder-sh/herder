import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct BackupTests {
    /// A vault replicating `devbox`.
    private func vault(_ hostId: HostId, role: Role? = .owner, connection: ConnectionState = .connected) -> Machine {
        var vault = machine(hostId, name: hostId, sessions: [], hosts: [
            FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: ""),
        ])
        vault.role = role
        vault.connection = connection
        vault.fingerprint = "fp-\(hostId)"
        return vault
    }

    private func fleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileWriteUnknown)
        }
        return fleet
    }

    @Test func aMachineBacksUpOnlyToConnectedVaultsThisDeviceOwns() throws {
        let fleet = try fleet()
        fleet.setMachinesForTesting([
            machine("devbox", name: "devbox", sessions: []),
            vault("home"),
            vault("shared", role: .member),
            vault("offline", connection: .disconnected(error: "gone")),
        ])
        #expect(fleet.backupVaults(for: "devbox").map(\.hostId) == ["home"])
        // A vault does not back up to itself.
        #expect(fleet.backupVaults(for: "home").isEmpty)
        // The vault a link names is found by its fingerprint, in any case.
        #expect(fleet.pairedVault(LinkedVault(address: "x:7447", fingerprint: "FP-HOME"))?.hostId == "home")
        #expect(fleet.pairedVault(LinkedVault(address: "x:7447", fingerprint: "fp-elsewhere")) == nil)
    }
}
