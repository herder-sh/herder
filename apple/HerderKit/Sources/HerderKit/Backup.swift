import Herder
import SwiftUI

/// Where a machine backs its sessions up, as it answers `get_vault_link`.
enum BackupLink: Equatable {
    /// It is a vault, which machines back up to.
    case vault
    /// It backs up to this vault.
    case linked(LinkedVault)
    /// It backs up nowhere.
    case nowhere
}

extension Fleet {
    /// Longest host name a vault takes as a user name.
    static let maxVaultHostName = 64

    /// The paired vaults `hostId` can back up to: connected, and owned by this device's user,
    /// since only an owner may mint a vault's host codes.
    func backupVaults(for hostId: HostId) -> [Machine] {
        let vaults = Set(vaults.map(\.hostId))
        return machines.filter {
            $0.hostId != hostId && vaults.contains($0.hostId) && $0.role == .owner && $0.connection == .connected
        }
    }

    /// The paired vault `link` names, by its fingerprint.
    func pairedVault(_ link: LinkedVault) -> Machine? {
        machines.first { $0.fingerprint.caseInsensitiveCompare(link.fingerprint) == .orderedSame }
    }

    /// Asks `hostId` where it backs up.
    func backupLink(of hostId: HostId) async throws -> BackupLink {
        let client = client
        let answer = try await answered(within: Self.vaultLinkTimeout, or: "did not say where it backs up") {
            try await client.send(hostId: hostId, command: .getVaultLink)
        }
        guard case .vaultLink(let isVault, let vault, _) = answer else {
            throw HerderError.Local(detail: "the machine did not say where it backs up")
        }
        if isVault { return .vault }
        return vault.map(BackupLink.linked) ?? .nowhere
    }

    /// Backs `host` up to `vault` from now on: a host-only code from the vault, named after
    /// the host, handed to the host with the vault's addresses.
    func backUp(_ host: Machine, to vault: Machine) async throws {
        let name = String(host.name.trimmingCharacters(in: .whitespaces).prefix(Self.maxVaultHostName))
        guard case .hostPairing(let code, _) = try await client.send(
            hostId: vault.hostId, command: .pairVaultHost(hostName: name))
        else { throw HerderError.Local(detail: "the vault did not mint a code") }
        _ = try await client.send(
            hostId: host.hostId,
            command: .linkVault(addresses: vault.addresses, fingerprint: vault.fingerprint, pairingCode: code))
    }

    /// Stops `hostId` backing up to `link`, and unpairs it from that vault when it is paired
    /// here, so the host's key no longer opens it. What the vault holds stays there.
    func stopBackingUp(_ hostId: HostId, from link: LinkedVault) async throws {
        _ = try await client.send(hostId: hostId, command: .unlinkVault)
        if let vault = pairedVault(link), vault.role == .owner {
            _ = try await client.send(hostId: vault.hostId, command: .revokeVaultHost(hostId: hostId))
        }
    }
}

/// A machine's backup in its settings: where it backs up, and backing it up to a vault this
/// device owns, or stopping.
struct BackupField: View {
    let fleet: Fleet
    let machine: Machine
    @State private var link: BackupLink?
    @State private var working = false
    @State private var confirmingStop = false
    @State private var error: String?

    var body: some View {
        Field(label: "Backup",
              hint: "A machine that backs up replicates every session to the vault, which keeps them when the machine is gone.") {
            VStack(alignment: .leading, spacing: 10) {
                switch link {
                case nil:
                    Text("Checking…").foregroundStyle(Theme.tertiary)
                case .vault:
                    Text("This machine is a vault.").foregroundStyle(Theme.secondary)
                case .linked(let vault):
                    DetailRow(label: "Backs up to", value: fleet.pairedVault(vault)?.name ?? vault.address)
                    ActionButton(title: "Stop Backing Up", style: .secondary) { confirmingStop = true }
                        .frame(maxWidth: 200)
                        .disabled(working)
                case .nowhere:
                    let vaults = fleet.backupVaults(for: machine.hostId)
                    if vaults.isEmpty {
                        Text("Backs up nowhere. Pair this device with a vault as its owner to back this machine up.")
                            .foregroundStyle(Theme.tertiary)
                    } else {
                        Text("Backs up nowhere.").foregroundStyle(Theme.secondary)
                        ForEach(vaults, id: \.hostId) { vault in
                            ActionButton(title: "Back Up to \(vault.name)", style: .secondary) {
                                run { try await fleet.backUp(machine, to: vault) }
                            }
                            .frame(maxWidth: 280)
                            .disabled(working)
                        }
                    }
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                }
            }
            .font(.subheadline)
            .padding(12)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        }
        .task(id: machine.hostId) { await refresh() }
        .confirmationDialog("Stop backing \(machine.name) up?", isPresented: $confirmingStop, titleVisibility: .visible) {
            Button("Stop Backing Up", role: .destructive) {
                guard case .linked(let vault) = link else { return }
                run { try await fleet.stopBackingUp(machine.hostId, from: vault) }
            }
        } message: {
            Text("What the vault holds stays there.")
        }
    }

    private func refresh() async {
        do {
            link = try await fleet.backupLink(of: machine.hostId)
        } catch {
            self.error = describe(error)
        }
    }

    private func run(_ action: @escaping () async throws -> Void) {
        working = true
        error = nil
        Task {
            do {
                try await action()
                await refresh()
            } catch {
                self.error = describe(error)
            }
            working = false
        }
    }
}
