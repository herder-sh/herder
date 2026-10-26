import Herder
import SwiftUI

/// Where a machine's sessions get their secrets from. Secrets live in a vault, in sets it grants
/// to hosts, and a machine fetches them only from the vault it is linked to, with Secrets on.
enum MachineSecrets: Equatable {
    /// The machine has not said yet.
    case asking
    /// The machine is a vault: it keeps secrets for others.
    case vault
    /// It fetches secrets from `vault`, which grants it `sets` sets, when this device can ask.
    case on(vault: String, sets: Int?)
    /// It is linked to `vault`, with Secrets off.
    case off(vault: String)
    /// It is linked to no vault.
    case none

    /// `hostId`'s secrets, from where it says it backs up (`link`), the vault that names when it
    /// is paired here, and that vault's sets when they were listed.
    init(hostId: HostId, link: BackupLink?, paired: Machine?, sets: [SecretSet]?) {
        switch link {
        case nil: self = .asking
        case .vault: self = .vault
        case .nowhere: self = .none
        case .linked(let linked):
            let name = paired?.name ?? "its vault"
            self = linked.secrets
                ? .on(vault: name, sets: sets.map { $0.filter { $0.hosts.contains(hostId) }.count })
                : .off(vault: name)
        }
    }

    /// "2 sets granted to this machine".
    static func granted(_ sets: Int?) -> String {
        switch sets {
        case nil: ""
        case 0: "No sets granted to this machine yet"
        case 1: "1 set granted to this machine"
        case let sets?: "\(sets) sets granted to this machine"
        }
    }
}

/// A machine's Secrets section: the vault it fetches secrets from, with the sets granted to it
/// and the way to that vault's Secrets tab, and the switch; or the ways to get a vault, linked
/// for secrets only. Members see only that its owners keep this.
struct SecretsSection: View {
    let fleet: Fleet
    let machine: Machine
    @Binding var sheet: AppSheet?
    @Environment(\.pages) private var pages
    /// The paired vault's secrets, listed when this device owns it.
    @State private var secrets: VaultSecrets?
    @State private var working = false
    @State private var error: String?

    var body: some View {
        SettingsList(title: "Secrets") {
            if machine.role == .owner {
                owned
            } else {
                SettingRow(label: "Kept by its owners", hint: "Only they see where its secrets come from") { EmptyView() }
            }
        }
        .task(id: pairedOwnedVault?.hostId) {
            guard let vault = pairedOwnedVault else { return secrets = nil }
            let listed = VaultSecrets(fleet: fleet, vault: vault.hostId)
            await listed.reload()
            secrets = listed
        }
    }

    @ViewBuilder private var owned: some View {
        let link = fleet.backupLinks[machine.hostId]
        switch MachineSecrets(hostId: machine.hostId, link: link, paired: linkedVault, sets: listedSets) {
        case .asking:
            SettingRow(label: "Secrets from") { ProgressView().controlSize(.small) }
        case .vault:
            SettingRow(label: "Kept here", hint: "Open the vault to manage its sets") {
                PillButton(title: "Open Secrets") { pages.open(.vault(machine.hostId, tab: .secrets)) }
            }
        case .on(let vault, let sets):
            SettingRow(label: "Secrets from \(vault)",
                       hint: pairedOwnedVault == nil ? "A vault not paired with this device as its owner" : MachineSecrets.granted(sets)) {
                if let paired = pairedOwnedVault {
                    PillButton(title: "Open Secrets", prominent: true) { pages.open(.vault(paired.hostId, tab: .secrets)) }
                }
            }
            secretsSwitch(link, vault: vault)
        case .off(let vault):
            SettingRow(label: "Secrets off", hint: "Sessions here fetch none from \(vault)") { EmptyView() }
            secretsSwitch(link, vault: vault)
        case .none:
            SettingRow(label: "No vault", hint: "Secrets live in a vault; this machine needs one") { EmptyView() }
            VaultChoices(fleet: fleet, machine: machine, sheet: $sheet, use: "Use for Secrets", working: working) { vault in
                run { try await fleet.link(machine, to: vault, backup: false, secrets: true) }
            }
        }
        if let error {
            RowDivider()
            Text(error).font(.footnote).foregroundStyle(Theme.failure)
                .padding(.horizontal, 14).frame(minHeight: 36)
        }
    }

    @ViewBuilder private func secretsSwitch(_ link: BackupLink?, vault: String) -> some View {
        if case .linked(let linked) = link {
            RowDivider()
            VaultLinkSwitch(fleet: fleet, hostId: machine.hostId, vault: linked, key: \.secrets, label: "Fetch secrets",
                            hint: "Sessions here use the sets \(vault) grants it")
        }
    }

    /// The paired vault the machine is linked to.
    private var linkedVault: Machine? {
        guard case .linked(let linked) = fleet.backupLinks[machine.hostId] else { return nil }
        return fleet.pairedVault(linked)
    }

    /// The linked vault, when this device owns it and can list its secrets.
    private var pairedOwnedVault: Machine? {
        linkedVault.flatMap { $0.role == .owner && $0.connection == .connected ? $0 : nil }
    }

    private var listedSets: [SecretSet]? {
        guard let secrets, secrets.vault == pairedOwnedVault?.hostId, case .loaded(let list) = secrets.list else { return nil }
        return list.sets
    }

    private func run(_ action: @escaping () async throws -> Void) {
        working = true
        error = nil
        Task {
            do {
                try await action()
                try await fleet.backupLink(of: machine.hostId)
            } catch {
                self.error = describe(error)
            }
            working = false
        }
    }
}
