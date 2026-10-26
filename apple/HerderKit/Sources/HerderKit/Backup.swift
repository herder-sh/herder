import Herder
import SwiftUI

/// Where a machine backs its sessions up, as it answers `get_vault_link`.
enum BackupLink: Equatable {
    /// It is a vault, which machines back up to.
    case vault
    /// It backs up to this vault.
    case linked(LinkedVault)
    /// It backs up nowhere; `localVault` when a vault is installed on its machine, to pair
    /// and link.
    case nowhere(localVault: Bool)
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

    /// Asks `hostId` where it backs up, and keeps the answer in `backupLinks`.
    @discardableResult
    func backupLink(of hostId: HostId) async throws -> BackupLink {
        let client = client
        let answer = try await answered(within: Self.vaultLinkTimeout, or: "did not say where it backs up") {
            try await client.send(hostId: hostId, command: .getVaultLink)
        }
        guard case .vaultLink(let isVault, let vault, _, let localVault) = answer else {
            throw HerderError.Local(detail: "the machine did not say where it backs up")
        }
        let link: BackupLink = isVault ? .vault : vault.map(BackupLink.linked) ?? .nowhere(localVault: localVault)
        backupLinks[hostId] = link
        return link
    }

    /// Links `host` to `vault` from now on, backing up and fetching secrets: a host-only code
    /// from the vault, named after the host, handed to the host with the vault's addresses.
    func backUp(_ host: Machine, to vault: Machine) async throws {
        let name = String(host.name.trimmingCharacters(in: .whitespaces).prefix(Self.maxVaultHostName))
        guard case .hostPairing(let code, _) = try await client.send(
            hostId: vault.hostId, command: .pairVaultHost(hostName: name))
        else { throw HerderError.Local(detail: "the vault did not mint a code") }
        _ = try await client.send(
            hostId: host.hostId,
            command: .linkVault(
                addresses: vault.addresses, fingerprint: vault.fingerprint, pairingCode: code,
                backup: true, secrets: true))
    }

    /// Sets whether `hostId` backs up to the vault it is linked to, and fetches secrets from
    /// it, keeping the link.
    func setVaultLink(_ hostId: HostId, backup: Bool, secrets: Bool) async throws {
        _ = try await client.send(hostId: hostId, command: .setVaultLink(backup: backup, secrets: secrets))
    }

    /// Stops `hostId` backing up to `link`, and unpairs it from that vault when it is paired
    /// here, so the host's key no longer opens it. What the vault holds stays there.
    func stopBackingUp(_ hostId: HostId, from link: LinkedVault) async throws {
        _ = try await client.send(hostId: hostId, command: .unlinkVault)
        if let vault = pairedVault(link), vault.role == .owner {
            _ = try await client.send(hostId: vault.hostId, command: .revokeVaultHost(hostId: hostId))
        }
    }

    /// How far the vault has got with a host's sessions, from what it reports of the host:
    /// "Caught up · 3m ago" while its last batch came in close behind the host.
    nonisolated static func freshness(_ replication: HostReplication?, now: Date) -> String {
        guard let replication else { return "" }
        guard let last = replication.lastEventAt.flatMap(Timestamp.date) else { return "Nothing backed up yet" }
        let age = "\(Timestamp.age(last, now: now)) ago"
        guard let lag = replication.lagMs, lag <= caughtUpLagMs else { return "Last change held \(age)" }
        return "Caught up · \(age)"
    }

    /// The most a batch may lag the host for the vault to count as caught up with it.
    nonisolated static let caughtUpLagMs: UInt64 = 60_000
}

/// A machine's Backup section: the vault it backs up to and how far that got, its Back up and
/// Secrets switches, whether it takes handoffs, and stopping; or linking it to a vault this
/// device owns, or running one on the machine. It shows where the machine last said it backs up while it asks again.
struct BackupSection: View {
    let fleet: Fleet
    let machine: Machine
    @State private var working = false
    @State private var confirmingStop = false
    @State private var error: String?
    @State private var saves = Autosave()
    /// Takes handoffs as just switched, until the vault's status says so.
    @State private var handoffs: Bool?

    var body: some View {
        let link = fleet.backupLinks[machine.hostId]
        SettingsList(title: "Backup") {
            switch link {
            case nil:
                SettingRow(label: "Backs up to") { ProgressView().controlSize(.small) }
            case .vault:
                SettingRow(label: "This machine is a vault") { EmptyView() }
            case .linked(let vault):
                linked(vault)
            case .nowhere(let localVault):
                if machine.role == .owner {
                    runVault(installed: localVault)
                    RowDivider()
                }
                let vaults = fleet.backupVaults(for: machine.hostId)
                SettingRow(label: "Backs up nowhere",
                           hint: vaults.isEmpty ? "Pair this device with a vault as its owner to back this machine up" : "") {
                    EmptyView()
                }
                ForEach(vaults, id: \.hostId) { vault in
                    RowDivider()
                    SettingRow(label: vault.name, hint: "A vault you own") {
                        PillButton(title: "Back Up Here", prominent: true) {
                            run { try await fleet.backUp(machine, to: vault) }
                        }
                        .disabled(working)
                    }
                }
            }
            if let error {
                RowDivider()
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    .padding(.horizontal, 14).frame(minHeight: 36)
            }
        }
        .task(id: machine.hostId) { await refresh() }
        .confirmationDialog("Stop backing \(machine.name) up?", isPresented: $confirmingStop, titleVisibility: .visible) {
            Button("Stop Backing Up", role: .destructive) {
                guard case .linked(let vault) = fleet.backupLinks[machine.hostId] else { return }
                run { try await fleet.stopBackingUp(machine.hostId, from: vault) }
            }
        } message: {
            Text("What the vault holds stays there.")
        }
    }

    /// The vault, how far it got with this machine, the switches, and Stop. The paired vault
    /// reports how far it got and whether the machine takes handoffs; one not paired here, nothing.
    @ViewBuilder private func linked(_ vault: LinkedVault) -> some View {
        let paired = fleet.pairedVault(vault)
        let replication = paired?.vault?.hosts.first { $0.hostId == machine.hostId }
        SettingRow(label: paired.map { "Backs up to \($0.name)" } ?? "Backs up to a vault not paired with this device",
                   hint: Fleet.freshness(replication, now: .now)) {
            PillButton(title: "Stop…") { confirmingStop = true }.disabled(working)
        }
        RowDivider()
        SettingRow(label: "Back up sessions", hint: "Every session is replicated to the vault", saved: saves["backup"]) {
            Toggle("Back up sessions", isOn: switchBinding(vault, \.backup, row: "backup")).labelsHidden().toggleStyle(.switch)
                .disabled(working)
        }
        RowDivider()
        SettingRow(label: "Secrets", hint: "Sessions here fetch secrets from the vault", saved: saves["secrets"]) {
            Toggle("Secrets", isOn: switchBinding(vault, \.secrets, row: "secrets")).labelsHidden().toggleStyle(.switch)
                .disabled(working)
        }
        if let paired, paired.role == .owner, let replication {
            RowDivider()
            SettingRow(label: "Takes handoffs from other machines",
                       hint: "It may read their sessions from the vault", saved: saves["handoffs"]) {
                Toggle("Takes handoffs from other machines", isOn: Binding(get: { handoffs ?? replication.read }, set: { on in
                    handoffs = on
                    Task {
                        await saves.save("handoffs") {
                            try await fleet.setTakesHandoffs(on, host: machine.hostId, vault: paired.hostId)
                        }
                        if case .failed = saves["handoffs"] { handoffs = nil }
                    }
                }))
                .labelsHidden()
                .toggleStyle(.switch)
                .accessibilityIdentifier("takes-handoffs")
            }
            // The vault's status follows the change; until then the switch shows it.
            .onChange(of: replication.read) { handoffs = nil }
        }
    }

    /// Running a vault on this machine, or pairing and linking the one installed there by
    /// hand, as an Activity job: its step while it runs, the command to run by hand when the
    /// machine's herder cannot do it.
    @ViewBuilder private func runVault(installed: Bool) -> some View {
        let key = JobKey(hostId: machine.hostId, kind: .vault)
        let progress = fleet.activity[key] == nil ? nil : fleet.progress(key)
        SettingRow(label: installed ? "A vault is installed on this machine" : "No vault",
                   hint: progress.map { $0.detail(machine: machine.name, before: nil) }
                       ?? (installed ? "Pair this device with it and link the machine to it"
                           : "Keeps this machine’s secrets; it runs next to its daemon")) {
            if let progress, !progress.finished {
                JobBadge(progress: progress)
            } else {
                PillButton(title: installed ? "Pair and Link" : "Run a Vault on This Machine", prominent: true) {
                    fleet.setUpVault(on: machine.hostId)
                }
                .accessibilityIdentifier("run-vault")
            }
        }
        if progress == .needsNewerHerder {
            RowDivider()
            VaultInstallCommand().padding(.horizontal, 14).frame(minHeight: 36)
        }
    }

    /// One of `vault`'s switches, set on the machine when flipped; back as the machine has it
    /// if that fails.
    private func switchBinding(_ vault: LinkedVault, _ key: WritableKeyPath<LinkedVault, Bool>, row: String) -> Binding<Bool> {
        Binding(
            get: { vault[keyPath: key] },
            set: { on in
                var switched = vault
                switched[keyPath: key] = on
                fleet.backupLinks[machine.hostId] = .linked(switched)
                Task {
                    await saves.save(row) {
                        try await fleet.setVaultLink(machine.hostId, backup: switched.backup, secrets: switched.secrets)
                    }
                    if case .failed = saves[row] { await refresh() }
                }
            })
    }

    /// Asks the machine again; a failure shows only while there is nothing known to show.
    private func refresh() async {
        do {
            try await fleet.backupLink(of: machine.hostId)
        } catch where fleet.backupLinks[machine.hostId] == nil {
            self.error = describe(error)
        } catch {}
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
