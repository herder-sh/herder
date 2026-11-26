import Herder
import SwiftUI

/// Where a machine's sessions get their secrets from. Secrets live in a hub, in sets it grants
/// to hosts, and a machine fetches them only from the hub it is linked to, with Secrets on.
enum MachineSecrets: Equatable {
    /// The machine has not said yet.
    case asking
    /// The machine is a hub: it keeps secrets for others.
    case hub
    /// It fetches secrets from `hub`, which grants it `sets` sets, when this device can ask.
    case on(hub: String, sets: Int?)
    /// It is linked to `hub`, with Secrets off.
    case off(hub: String)
    /// It is linked to no hub.
    case none

    /// `hostId`'s secrets, from where it says it backs up (`link`), the hub that names when it
    /// is paired here, and that hub's sets when they were listed.
    init(hostId: HostId, link: BackupLink?, paired: Machine?, sets: [SecretSet]?) {
        switch link {
        case nil: self = .asking
        case .hub: self = .hub
        case .nowhere: self = .none
        case .linked(let linked):
            let name = paired?.name ?? "its hub"
            self = linked.secrets
                ? .on(hub: name, sets: sets.map { $0.filter { $0.hosts.contains(hostId) }.count })
                : .off(hub: name)
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

/// A machine's Secrets section: the hub it fetches secrets from, with the sets granted to it
/// and the way to that hub's Secrets tab, and the switch; or the ways to get a hub, linked
/// for secrets only. Members see only that its owners keep this.
struct SecretsSection: View {
    let fleet: Fleet
    let machine: Machine
    @Binding var sheet: AppSheet?
    @Environment(\.pages) private var pages
    /// The paired hub's secrets, listed when this device owns it.
    @State private var secrets: HubSecrets?
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
        .task(id: pairedOwnedHub?.hostId) {
            guard let hub = pairedOwnedHub else { return secrets = nil }
            let listed = HubSecrets(fleet: fleet, hub: hub.hostId)
            await listed.reload()
            secrets = listed
        }
    }

    @ViewBuilder private var owned: some View {
        let link = fleet.backupLinks[machine.hostId]
        switch MachineSecrets(hostId: machine.hostId, link: link, paired: linkedHub, sets: listedSets) {
        case .asking:
            SettingRow(label: "Secrets from") { ProgressView().controlSize(.small) }
        case .hub:
            SettingRow(label: "Kept here", hint: "Open the hub to manage its sets") {
                PillButton(title: "Open Secrets") { pages.open(.hub(machine.hostId, tab: .secrets)) }
            }
        case .on(let hub, let sets):
            SettingRow(label: "Secrets from \(hub)",
                       hint: pairedOwnedHub == nil ? "A hub not paired with this device as its owner" : MachineSecrets.granted(sets)) {
                if let paired = pairedOwnedHub {
                    PillButton(title: "Open Secrets", prominent: true) { pages.open(.hub(paired.hostId, tab: .secrets)) }
                }
            }
            secretsSwitch(link, hub: hub)
        case .off(let hub):
            SettingRow(label: "Secrets off", hint: "Sessions here fetch none from \(hub)") { EmptyView() }
            secretsSwitch(link, hub: hub)
        case .none:
            SettingRow(label: "No hub", hint: "Secrets live in a hub; this machine needs one") { EmptyView() }
            HubChoices(fleet: fleet, machine: machine, sheet: $sheet, use: "Use for Secrets", working: working) { hub in
                run { try await fleet.link(machine, to: hub, backup: false, secrets: true) }
            }
        }
        if let error {
            RowDivider()
            Text(error).font(.footnote).foregroundStyle(Theme.failure)
                .padding(.horizontal, 14).frame(minHeight: 36)
        }
    }

    @ViewBuilder private func secretsSwitch(_ link: BackupLink?, hub: String) -> some View {
        if case .linked(let linked) = link {
            RowDivider()
            HubLinkSwitch(fleet: fleet, hostId: machine.hostId, hub: linked, key: \.secrets, label: "Fetch secrets",
                            hint: "Sessions here use the sets \(hub) grants it")
        }
    }

    /// The paired hub the machine is linked to.
    private var linkedHub: Machine? {
        guard case .linked(let linked) = fleet.backupLinks[machine.hostId] else { return nil }
        return fleet.pairedHub(linked)
    }

    /// The linked hub, when this device owns it and can list its secrets.
    private var pairedOwnedHub: Machine? {
        linkedHub.flatMap { $0.role == .owner && $0.connection == .connected ? $0 : nil }
    }

    private var listedSets: [SecretSet]? {
        guard let secrets, secrets.hub == pairedOwnedHub?.hostId, case .loaded(let list) = secrets.list else { return nil }
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
