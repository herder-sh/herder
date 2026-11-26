import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct BackupTests {
    /// A hub replicating `devbox`.
    private func hub(_ hostId: HostId, role: Role? = .owner, connection: ConnectionState = .connected) -> Machine {
        var hub = machine(hostId, name: hostId, sessions: [], hosts: [
            FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: ""),
        ])
        hub.role = role
        hub.connection = connection
        hub.fingerprint = "fp-\(hostId)"
        return hub
    }

    private func fleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileWriteUnknown)
        }
        return fleet
    }

    @Test func aMachineBacksUpOnlyToConnectedHubsThisDeviceOwns() throws {
        let fleet = try fleet()
        fleet.setMachinesForTesting([
            machine("devbox", name: "devbox", sessions: []),
            hub("home"),
            hub("shared", role: .member),
            hub("offline", connection: .disconnected(error: "gone")),
        ])
        #expect(fleet.backupHubs(for: "devbox").map(\.hostId) == ["home"])
        // A hub does not back up to itself.
        #expect(fleet.backupHubs(for: "home").isEmpty)
        // The hub a link names is found by its fingerprint, in any case.
        #expect(fleet.pairedHub(LinkedHub(address: "x:7447", fingerprint: "FP-HOME", backup: true, secrets: true))?.hostId == "home")
        #expect(fleet.pairedHub(LinkedHub(address: "x:7447", fingerprint: "fp-elsewhere", backup: true, secrets: false)) == nil)
    }

    @Test func aMachinesSecretsComeFromTheHubItIsLinkedToWithSecretsOn() {
        let home = hub("home")
        let link = LinkedHub(address: "10.0.0.9:7447", fingerprint: "fp-home", backup: false, secrets: true)
        let sets = [SecretSet(name: "prod", hosts: ["devbox", "other"], hostUse: []),
                    SecretSet(name: "staging", hosts: ["devbox"], hostUse: []),
                    SecretSet(name: "bot", hosts: [], hostUse: ["devbox"])]
        func secrets(_ link: BackupLink?, paired: Machine? = nil, sets: [SecretSet]? = nil) -> MachineSecrets {
            MachineSecrets(hostId: "devbox", link: link, paired: paired, sets: sets)
        }
        #expect(secrets(nil) == .asking)
        #expect(secrets(.hub) == .hub)
        #expect(secrets(.nowhere(localHub: false)) == .none)
        #expect(secrets(.nowhere(localHub: true)) == .none)
        // Sets granted to the host only for its own use give its sessions nothing.
        #expect(secrets(.linked(link), paired: home, sets: sets) == .on(hub: "home", sets: 2))
        // A hub not paired here is never named by a raw address, and its sets are unknown.
        #expect(secrets(.linked(link)) == .on(hub: "its hub", sets: nil))
        var off = link
        off.secrets = false
        #expect(secrets(.linked(off), paired: home, sets: sets) == .off(hub: "home"))
        #expect(MachineSecrets.granted(nil).isEmpty)
        #expect(MachineSecrets.granted(0) == "No sets granted to this machine yet")
        #expect(MachineSecrets.granted(1) == "1 set granted to this machine")
        #expect(MachineSecrets.granted(2) == "2 sets granted to this machine")
    }
}
