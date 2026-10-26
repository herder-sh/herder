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
        #expect(fleet.pairedVault(LinkedVault(address: "x:7447", fingerprint: "FP-HOME", backup: true, secrets: true))?.hostId == "home")
        #expect(fleet.pairedVault(LinkedVault(address: "x:7447", fingerprint: "fp-elsewhere", backup: true, secrets: false)) == nil)
    }

    @Test func aMachinesSecretsComeFromTheVaultItIsLinkedToWithSecretsOn() {
        let home = vault("home")
        let link = LinkedVault(address: "10.0.0.9:7447", fingerprint: "fp-home", backup: false, secrets: true)
        let sets = [SecretSet(name: "prod", hosts: ["devbox", "other"], hostUse: []),
                    SecretSet(name: "staging", hosts: ["devbox"], hostUse: []),
                    SecretSet(name: "bot", hosts: [], hostUse: ["devbox"])]
        func secrets(_ link: BackupLink?, paired: Machine? = nil, sets: [SecretSet]? = nil) -> MachineSecrets {
            MachineSecrets(hostId: "devbox", link: link, paired: paired, sets: sets)
        }
        #expect(secrets(nil) == .asking)
        #expect(secrets(.vault) == .vault)
        #expect(secrets(.nowhere(localVault: false)) == .none)
        #expect(secrets(.nowhere(localVault: true)) == .none)
        // Sets granted to the host only for its own use give its sessions nothing.
        #expect(secrets(.linked(link), paired: home, sets: sets) == .on(vault: "home", sets: 2))
        // A vault not paired here is never named by a raw address, and its sets are unknown.
        #expect(secrets(.linked(link)) == .on(vault: "its vault", sets: nil))
        var off = link
        off.secrets = false
        #expect(secrets(.linked(off), paired: home, sets: sets) == .off(vault: "home"))
        #expect(MachineSecrets.granted(nil).isEmpty)
        #expect(MachineSecrets.granted(0) == "No sets granted to this machine yet")
        #expect(MachineSecrets.granted(1) == "1 set granted to this machine")
        #expect(MachineSecrets.granted(2) == "2 sets granted to this machine")
    }
}
