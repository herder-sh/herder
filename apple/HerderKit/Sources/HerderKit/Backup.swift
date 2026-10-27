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

    /// Links `host` to `vault` from now on, backing it up there or not and fetching secrets from
    /// it or not: a host-only code from the vault, named after the host, handed to the host with
    /// the vault's addresses.
    func link(_ host: Machine, to vault: Machine, backup: Bool, secrets: Bool) async throws {
        let name = String(host.name.trimmingCharacters(in: .whitespaces).prefix(Self.maxVaultHostName))
        guard case .hostPairing(let code, _) = try await client.send(
            hostId: vault.hostId, command: .pairVaultHost(hostName: name))
        else { throw HerderError.Local(detail: "the vault did not mint a code") }
        _ = try await client.send(
            hostId: host.hostId,
            command: .linkVault(
                addresses: vault.addresses, fingerprint: vault.fingerprint, pairingCode: code,
                backup: backup, secrets: secrets))
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

/// A machine's Backup section: the vault it backs up to and how far that got, its Back up
/// switch, whether it takes handoffs, and stopping; or the ways to get a vault to back up to.
/// It shows where the machine last said it backs up while it asks again.
struct BackupSection: View {
    let fleet: Fleet
    let machine: Machine
    @Binding var sheet: AppSheet?
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
            case .nowhere:
                SettingRow(label: "Not backed up", hint: "Its sessions live only on \(machine.name)") { EmptyView() }
                VaultChoices(fleet: fleet, machine: machine, sheet: $sheet, use: "Back Up Here", working: working) { vault in
                    run { try await fleet.link(machine, to: vault, backup: true, secrets: true) }
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
            Button("Unlink", role: .destructive) {
                guard case .linked(let vault) = fleet.backupLinks[machine.hostId] else { return }
                run { try await fleet.stopBackingUp(machine.hostId, from: vault) }
            }
        } message: {
            Text("It no longer backs up there nor fetches secrets from it. What the vault holds stays there.")
        }
    }

    /// The vault, how far it got with this machine, the Back up switch, and Stop. The paired
    /// vault reports how far it got and whether the machine takes handoffs; one not paired here,
    /// nothing.
    @ViewBuilder private func linked(_ vault: LinkedVault) -> some View {
        let paired = fleet.pairedVault(vault)
        let replication = paired?.vault?.hosts.first { $0.hostId == machine.hostId }
        let name = paired?.name ?? "a vault not paired with this device"
        SettingRow(label: vault.backup ? "Backs up to \(name)" : "Not backed up",
                   hint: vault.backup ? Fleet.freshness(replication, now: .now) : "Linked to \(name) for secrets only") {
            PillButton(title: "Unlink…") { confirmingStop = true }.disabled(working)
        }
        RowDivider()
        VaultLinkSwitch(fleet: fleet, hostId: machine.hostId, vault: vault, key: \.backup, label: "Back up sessions",
                        hint: "Every session is replicated to the vault")
            .disabled(working)
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
                .toggleStyle(.settings)
                .accessibilityIdentifier("takes-handoffs")
            }
            // The vault's status follows the change; until then the switch shows it.
            .onChange(of: replication.read) { handoffs = nil }
        }
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

/// One of a linked machine's switches, Back up or Secrets, set on the machine when flipped;
/// back as the machine has it if that fails.
struct VaultLinkSwitch: View {
    let fleet: Fleet
    let hostId: HostId
    let vault: LinkedVault
    let key: WritableKeyPath<LinkedVault, Bool>
    let label: String
    let hint: String
    @State private var saves = Autosave()

    var body: some View {
        SettingRow(label: label, hint: hint, saved: saves["switch"]) {
            Toggle(label, isOn: Binding(get: { vault[keyPath: key] }, set: { flip($0) })).labelsHidden().toggleStyle(.settings)
        }
    }

    private func flip(_ on: Bool) {
        var switched = vault
        switched[keyPath: key] = on
        fleet.backupLinks[hostId] = .linked(switched)
        Task {
            await saves.save("switch") {
                try await fleet.setVaultLink(hostId, backup: switched.backup, secrets: switched.secrets)
            }
            if case .failed = saves["switch"] { _ = try? await fleet.backupLink(of: hostId) }
        }
    }
}

/// The ways to give a machine linked to no vault one: each connected vault this device owns,
/// with `use` linking to it, or pairing one; and running a vault on the machine itself with
/// `herder vault install`, then pairing it.
struct VaultChoices: View {
    let fleet: Fleet
    let machine: Machine
    @Binding var sheet: AppSheet?
    let use: String
    let working: Bool
    let link: (Machine) -> Void

    static let installCommand = "herder vault install"

    var body: some View {
        let vaults = fleet.backupVaults(for: machine.hostId)
        ForEach(vaults, id: \.hostId) { vault in
            RowDivider()
            SettingRow(label: vault.name, hint: "A vault you own") {
                PillButton(title: use, prominent: true) { link(vault) }.disabled(working)
            }
        }
        if vaults.isEmpty {
            RowDivider()
            SettingRow(label: "Use a vault you have", hint: "Pair it with this device as its owner") {
                PillButton(title: "Pair Vault…") { sheet = .pair(.vault) }
            }
        }
        RowDivider()
        runVault
    }

    /// Running a vault on the machine, or pairing and linking the one installed there by hand,
    /// as an Activity job: its step while it runs; the command to run there by hand when its
    /// herder cannot do it.
    @ViewBuilder private var runVault: some View {
        let installed = fleet.backupLinks[machine.hostId] == .nowhere(localVault: true)
        let key = JobKey(hostId: machine.hostId, kind: .vault)
        let progress = fleet.activity[key] == nil ? nil : fleet.progress(key)
        SettingRow(label: installed ? "A vault is installed on \(machine.name)" : "Run a vault on \(machine.name)",
                   hint: progress.map { $0.detail(machine: machine.name, before: nil) }
                       ?? (installed ? "Pair this device with it and link \(machine.name) to it"
                           : "It runs next to the daemon, and this device pairs with it")) {
            if let progress, !progress.finished {
                JobBadge(progress: progress)
            } else {
                PillButton(title: installed ? "Pair and Link" : "Run a Vault on This Machine", prominent: true) {
                    fleet.setUpVault(on: machine.hostId)
                }
                .disabled(working)
                .accessibilityIdentifier("run-vault")
            }
        }
        if progress == .needsNewerHerder {
            VaultInstallCommand()
                .padding(.horizontal, Theme.cellPadding)
                .padding(.bottom, 10)
            RowDivider()
            SettingRow(label: "Then pair with the vault", hint: "With the code it prints") {
                PillButton(title: "Pair Vault…") { sheet = .pair(.vault) }
            }
        }
    }
}

/// `herder vault install`, to copy and run on a machine whose herder cannot install a vault
/// from here; it prints the code to pair with the vault.
struct VaultInstallCommand: View {
    var body: some View {
        HStack(spacing: 8) {
            Text(VaultChoices.installCommand).font(Theme.mono).foregroundStyle(Theme.text).textSelection(.enabled)
            Spacer(minLength: 0)
            IconButton(symbol: "doc.on.doc", help: "Copy") { Clipboard.string = VaultChoices.installCommand }
        }
        .padding(.leading, 10)
        .padding(.trailing, 4)
        .padding(.vertical, 4)
        .background(Theme.raised, in: .rect(cornerRadius: 8))
    }
}
