import Herder
import SwiftUI

/// Where a machine backs its sessions up, as it answers `get_hub_link`.
enum BackupLink: Equatable {
    /// It is a hub, which machines back up to.
    case hub
    /// It backs up to this hub.
    case linked(LinkedHub)
    /// It backs up nowhere; `localHub` when a hub is installed on its machine, to claim
    /// and link.
    case nowhere(localHub: Bool)
}

extension Fleet {
    /// Longest host name a hub takes as a user name.
    static let maxHubHostName = 64

    /// The paired hubs `hostId` can back up to: connected, and owned by this device's user,
    /// since only an owner may mint a hub's host codes.
    func backupHubs(for hostId: HostId) -> [Machine] {
        let hubs = Set(hubs.map(\.hostId))
        return machines.filter {
            $0.hostId != hostId && hubs.contains($0.hostId) && $0.role == .owner && $0.connection == .connected
        }
    }

    /// The paired hub `link` names, by its fingerprint.
    func pairedHub(_ link: LinkedHub) -> Machine? {
        machines.first { $0.fingerprint.caseInsensitiveCompare(link.fingerprint) == .orderedSame }
    }

    /// Asks `hostId` where it backs up, and keeps the answer in `backupLinks`.
    @discardableResult
    func backupLink(of hostId: HostId) async throws -> BackupLink {
        let client = client
        let answer = try await answered(within: Self.hubLinkTimeout, or: "did not say where it backs up") {
            try await client.send(hostId: hostId, command: .getHubLink)
        }
        guard case .hubLink(let isHub, let hub, _, let localHub) = answer else {
            throw HerderError.Local(detail: "the machine did not say where it backs up")
        }
        let link: BackupLink = isHub ? .hub : hub.map(BackupLink.linked) ?? .nowhere(localHub: localHub)
        backupLinks[hostId] = link
        return link
    }

    /// Links `host` to `hub` from now on, backing it up there or not and fetching secrets from
    /// it or not: a host-only code from the hub, named after the host, handed to the host with
    /// the hub's addresses.
    func link(_ host: Machine, to hub: Machine, backup: Bool, secrets: Bool) async throws {
        let name = String(host.name.trimmingCharacters(in: .whitespaces).prefix(Self.maxHubHostName))
        guard case .hostPairing(let code, _) = try await client.send(
            hostId: hub.hostId, command: .pairHubHost(hostName: name))
        else { throw HerderError.Local(detail: "the hub did not mint a code") }
        _ = try await client.send(
            hostId: host.hostId,
            command: .linkHub(
                addresses: hub.addresses, fingerprint: hub.fingerprint, pairingCode: code,
                backup: backup, secrets: secrets))
    }

    /// Sets whether `hostId` backs up to the hub it is linked to, and fetches secrets from
    /// it, keeping the link.
    func setHubLink(_ hostId: HostId, backup: Bool, secrets: Bool) async throws {
        _ = try await client.send(hostId: hostId, command: .setHubLink(backup: backup, secrets: secrets))
    }

    /// Stops `hostId` backing up to `link`, and unpairs it from that hub when it is paired
    /// here, so the host's key no longer opens it. What the hub holds stays there.
    func stopBackingUp(_ hostId: HostId, from link: LinkedHub) async throws {
        _ = try await client.send(hostId: hostId, command: .unlinkHub)
        if let hub = pairedHub(link), hub.role == .owner {
            _ = try await client.send(hostId: hub.hostId, command: .revokeHubHost(hostId: hostId))
        }
    }

    /// How far the hub has got with a host's sessions, from what it reports of the host:
    /// "Caught up · 3m ago" while its last batch came in close behind the host.
    nonisolated static func freshness(_ replication: HostReplication?, now: Date) -> String {
        guard let replication else { return "" }
        guard let last = replication.lastEventAt.flatMap(Timestamp.date) else { return "Nothing backed up yet" }
        let age = "\(Timestamp.age(last, now: now)) ago"
        guard let lag = replication.lagMs, lag <= caughtUpLagMs else { return "Last change held \(age)" }
        return "Caught up · \(age)"
    }

    /// The most a batch may lag the host for the hub to count as caught up with it.
    nonisolated static let caughtUpLagMs: UInt64 = 60_000
}

/// A machine's Backup section: the hub it backs up to and how far that got, its Back up
/// switch, and stopping; or the ways to get a hub to back up to.
/// It shows where the machine last said it backs up while it asks again.
struct BackupSection: View {
    let fleet: Fleet
    let machine: Machine
    @Binding var sheet: AppSheet?
    @State private var working = false
    @State private var confirmingStop = false
    @State private var error: String?
    @State private var saves = Autosave()

    var body: some View {
        let link = fleet.backupLinks[machine.hostId]
        SettingsList(title: "Backup") {
            switch link {
            case nil:
                SettingRow(label: "Backs up to") { ProgressView().controlSize(.small) }
            case .hub:
                SettingRow(label: "This machine is a hub") { EmptyView() }
            case .linked(let hub):
                linked(hub)
            case .nowhere:
                SettingRow(label: "Not backed up", hint: "Its sessions live only on \(machine.name)") { EmptyView() }
                HubChoices(fleet: fleet, machine: machine, sheet: $sheet, use: "Back Up Here", working: working) { hub in
                    run { try await fleet.link(machine, to: hub, backup: true, secrets: true) }
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
                guard case .linked(let hub) = fleet.backupLinks[machine.hostId] else { return }
                run { try await fleet.stopBackingUp(machine.hostId, from: hub) }
            }
        } message: {
            Text("It no longer backs up there nor fetches secrets from it. What the hub holds stays there.")
        }
    }

    /// The hub, how far it got with this machine, the Back up switch, and Stop. The paired
    /// hub reports how far it got; one not paired here, nothing.
    @ViewBuilder private func linked(_ hub: LinkedHub) -> some View {
        let paired = fleet.pairedHub(hub)
        // How far it got moves with every event the hub takes in: `liveMachines`.
        let replication = fleet.liveMachines.first { $0.hostId == paired?.hostId }?.hub?.hosts
            .first { $0.hostId == machine.hostId }
        let name = paired?.name ?? "a hub not paired with this device"
        SettingRow(label: hub.backup ? "Backs up to \(name)" : "Not backed up",
                   hint: hub.backup ? Fleet.freshness(replication, now: .now) : "Linked to \(name) for secrets only") {
            PillButton(title: "Unlink…") { confirmingStop = true }.disabled(working)
        }
        RowDivider()
        HubLinkSwitch(fleet: fleet, hostId: machine.hostId, hub: hub, key: \.backup, label: "Back up sessions",
                        hint: "Every session is replicated to the hub")
            .disabled(working)
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
struct HubLinkSwitch: View {
    let fleet: Fleet
    let hostId: HostId
    let hub: LinkedHub
    let key: WritableKeyPath<LinkedHub, Bool>
    let label: String
    let hint: String
    @State private var saves = Autosave()

    var body: some View {
        SettingRow(label: label, hint: hint, saved: saves["switch"]) {
            Toggle(label, isOn: Binding(get: { hub[keyPath: key] }, set: { flip($0) })).labelsHidden().toggleStyle(.settings)
        }
    }

    private func flip(_ on: Bool) {
        var switched = hub
        switched[keyPath: key] = on
        fleet.backupLinks[hostId] = .linked(switched)
        Task {
            await saves.save("switch") {
                try await fleet.setHubLink(hostId, backup: switched.backup, secrets: switched.secrets)
            }
            if case .failed = saves["switch"] { _ = try? await fleet.backupLink(of: hostId) }
        }
    }
}

/// The ways to give a machine linked to no hub one: each connected hub this device owns,
/// with `use` linking to it, or pairing one; and running a hub on the machine itself with
/// `herder hub install`, then pairing it.
struct HubChoices: View {
    let fleet: Fleet
    let machine: Machine
    @Binding var sheet: AppSheet?
    let use: String
    let working: Bool
    let link: (Machine) -> Void

    static let installCommand = "herder hub install"

    var body: some View {
        let hubs = fleet.backupHubs(for: machine.hostId)
        ForEach(hubs, id: \.hostId) { hub in
            RowDivider()
            SettingRow(label: hub.name, hint: "A hub you own") {
                PillButton(title: use, prominent: true) { link(hub) }.disabled(working)
            }
        }
        if hubs.isEmpty {
            RowDivider()
            SettingRow(label: "Use a hub you have", hint: "Add it to your fleet as its owner") {
                PillButton(title: "Add Hub…") { sheet = .addMachine(.hasHerder) }
            }
        }
        RowDivider()
        runHub
    }

    /// Running a hub on the machine as an Activity job: its step while it runs; the command
    /// to run there by hand when its herder cannot do it. One installed there by hand is
    /// claimed with `herder claim` there, then used as any hub you own.
    @ViewBuilder private var runHub: some View {
        let key = JobKey(hostId: machine.hostId, kind: .hub)
        let progress = fleet.activity[key] == nil ? nil : fleet.progress(key)
        if fleet.backupLinks[machine.hostId] == .nowhere(localHub: true) && progress == nil {
            SettingRow(label: "A hub is installed on \(machine.name)",
                       hint: "Run \(MachineSource.claimCommand) there to own it, then use it here") {
                PillButton(title: "Add Hub…") { sheet = .addMachine(.hasHerder) }
            }
        } else {
            SettingRow(label: "Run a hub on \(machine.name)",
                       hint: progress.map { $0.detail(machine: machine.name, before: nil) }
                           ?? "It runs next to the daemon, and you approve it as its owner") {
                if let progress, !progress.finished {
                    JobBadge(progress: progress)
                } else {
                    PillButton(title: "Run a Hub on This Machine", prominent: true) {
                        fleet.setUpHub(on: machine.hostId)
                    }
                    .disabled(working)
                    .accessibilityIdentifier("run-hub")
                }
            }
        }
        if progress == .needsNewerHerder {
            HubInstallCommand()
                .padding(.horizontal, Theme.cellPadding)
                .padding(.bottom, 10)
            RowDivider()
            SettingRow(label: "Then add the hub to your fleet", hint: "With the link it prints") {
                PillButton(title: "Add Hub…") { sheet = .addMachine(.hasHerder) }
            }
        }
    }
}

/// `herder hub install`, to copy and run on a machine whose herder cannot install a hub
/// from here; it prints the link that makes you its owner.
struct HubInstallCommand: View {
    var body: some View {
        HStack(spacing: 8) {
            TranscriptText(HubChoices.installCommand, style: .footnote, monospaced: true)
            Spacer(minLength: 0)
            IconButton(symbol: "doc.on.doc", help: "Copy") { Clipboard.string = HubChoices.installCommand }
        }
        .padding(.leading, 10)
        .padding(.trailing, 4)
        .padding(.vertical, 4)
        .background(Theme.raised, in: .rect(cornerRadius: 8))
    }
}
