import Foundation
import Herder
import Observation
import SwiftUI

/// Signs a challenge with this device's herder passkey, behind Face ID. The app sets it once it
/// has a passkey; until then every change to people says one is needed.
typealias PasskeySigner = @MainActor (PasskeyChallenge) async throws -> PasskeyAssertion

extension EnvironmentValues {
    @Entry var signPasskey: PasskeySigner = { _ in
        throw HerderError.Local(detail: "Create your herder passkey first: it signs every change to people.")
    }
}

/// The fleet's people: each machine's, one person per person id. Every change carries an
/// assertion by your passkey over a challenge from the machine it goes to, then every machine
/// is asked again.
@MainActor @Observable
final class FleetPeople {
    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult

    private let send: Send
    private(set) var people: [HostId: [PersonInfo]] = [:]
    /// Why a machine did not list its people.
    private(set) var failures: [HostId: String] = [:]

    init(send: @escaping Send) {
        self.send = send
    }

    convenience init(fleet: Fleet) {
        let client = fleet.client
        self.init { try await client.send(hostId: $0, command: $1) }
    }

    /// The client this app pairs as, without its version: "herder-macos".
    nonisolated static var thisClient: String {
        #if os(macOS)
        "herder-macos"
        #else
        "herder-ios"
        #endif
    }

    /// A machine's one-time invite that lets someone join it with a passkey of their own.
    struct Invite: Equatable {
        let role: Role
        let machine: InviteLink.Machine
        let expiresAt: Date?
    }

    func reload(_ machines: [Machine]) async {
        for machine in machines where machine.connection == .connected {
            do {
                guard case .people(let people) = try await send(machine.hostId, .listPeople) else {
                    throw HerderError.Local(detail: "the machine did not list its people")
                }
                self.people[machine.hostId] = people
                failures[machine.hostId] = nil
            } catch {
                failures[machine.hostId] = describe(error)
            }
        }
    }

    /// The directory of `machines`, or nil before any of them listed its people. A vault's
    /// column is named apart from its machine's.
    func directory(_ machines: [Machine], localNames: Set<String> = MachineRow.localNames) -> Directory? {
        guard !people.isEmpty else { return nil }
        let local = MachineRow.thisMac(machines.filter { $0.hosts.isEmpty && $0.vault == nil }, names: localNames)
        let columns = machines.filter { people[$0.hostId] != nil }.map { machine in
            Directory.Column(id: machine.hostId, name: machine.hostId == local ? "this Mac" : UnpairedVault.name(of: machine),
                             vault: !machine.hosts.isEmpty || machine.vault != nil)
        }
        return .merging(people, columns: columns, me: Self.me(machines),
                        thisClient: Self.thisClient + "/")
    }

    /// You: the person the machines say this device acts for, the one most of them name.
    nonisolated static func me(_ machines: [Machine]) -> PersonId? {
        let votes = Dictionary(grouping: machines.compactMap(\.personId), by: \.self).mapValues(\.count)
        return votes.max { ($0.value, $1.key) < ($1.value, $0.key) }?.key
    }

    /// Gives `person` `role` on `hosts`, adding them where a machine does not know them yet, or
    /// takes their access there away for a nil role.
    func setRole(_ role: Role?, of person: Directory.Person, on hosts: [Directory.Column], machines: [Machine],
                 sign: PasskeySigner) async throws {
        let card = card(person.id)
        for host in hosts {
            let known = people[host.id]?.first { $0.personId == person.id }
            if let role {
                if known?.role == role { continue }
                if known == nil && card == nil {
                    throw HerderError.Local(detail: "\(person.name) has no passkey \(host.name) could check.")
                }
                let assertion = try await assertion(on: host.id, sign: sign)
                _ = try await send(host.id, .setPersonRole(personId: person.id, role: role,
                                                          person: known == nil ? card : nil, assertion: assertion))
            } else if known != nil {
                let assertion = try await assertion(on: host.id, sign: sign)
                _ = try await send(host.id, .revokePerson(personId: person.id, assertion: assertion))
            }
        }
        await reload(machines)
    }

    /// Folds `person`, a duplicate of you, `me`, into you on every machine that lists you both
    /// with you as owner: their devices become yours, and they are gone. One Face ID with their
    /// passkey signs every such machine's challenge, one after the other, proving it is you.
    func itsMe(_ person: Directory.Person, me: PersonId, machines: [Machine], sign: PasskeySigner) async throws {
        let hosts = machines.map(\.hostId).filter { hostId in
            let listed = people[hostId] ?? []
            return listed.contains { $0.personId == person.id }
                && listed.contains { $0.personId == me && $0.role == .owner }
        }
        guard !hosts.isEmpty else {
            throw HerderError.Local(detail: "You own no machine that knows \(person.name).")
        }
        var challenges: [PasskeyChallenge] = []
        for hostId in hosts {
            guard case .passkeyChallenge(let challenge, _) = try await send(hostId, .passkeyChallenge) else {
                throw HerderError.Local(detail: "the machine did not send a challenge")
            }
            challenges.append(challenge)
        }
        var batch = Data()
        for challenge in challenges {
            guard let bytes = Data(base64URL: challenge.challenge) else {
                throw HerderError.Local(detail: "the machine sent a challenge that is not base64url")
            }
            batch += bytes
        }
        let assertion = try await sign(PasskeyChallenge(rpId: challenges[0].rpId, challenge: batch.base64URLEncodedString(),
                                                        expiresAt: challenges.map(\.expiresAt).min() ?? ""))
        let theirs = people.values.flatMap(\.self).filter { $0.personId == person.id }.flatMap(\.passkeys)
        guard theirs.contains(where: { $0.credentialId == assertion.id }) else {
            throw HerderError.Local(detail: "That passkey is not \(person.name)'s; sign with the one they were made with.")
        }
        var failures: [String] = []
        for hostId in hosts {
            do {
                _ = try await send(hostId, .mergePerson(from: person.id, assertion: assertion))
            } catch {
                failures.append(describe(error))
            }
        }
        await reload(machines)
        if !failures.isEmpty { throw HerderError.Local(detail: failures.joined(separator: "\n")) }
    }

    /// Signs `device` out of every machine it paired with.
    func signOut(_ device: Directory.Device, machines: [Machine], sign: PasskeySigner) async throws {
        for (hostId, devices) in device.paired {
            for deviceId in devices {
                let assertion = try await assertion(on: hostId, sign: sign)
                _ = try await send(hostId, .revokeDevice(deviceId: deviceId, assertion: assertion))
            }
        }
        await reload(machines)
    }

    /// Mints an invite that lets someone join `hostId` as `role`; `name` is what the link calls
    /// the machine.
    func invite(_ role: Role, on hostId: HostId, name: String? = nil, sign: PasskeySigner) async throws -> Invite {
        let assertion = try await assertion(on: hostId, sign: sign)
        guard case .invite(let invite, let role, let fingerprint, let addresses, let expiresAt) =
            try await send(hostId, .invite(role: role, assertion: assertion))
        else {
            throw HerderError.Local(detail: "the machine did not mint an invite")
        }
        return Invite(role: role, machine: InviteLink.Machine(hosts: addresses, fingerprint: fingerprint, invite: invite, name: name),
                      expiresAt: Timestamp.date(expiresAt))
    }

    /// Who `personId` is, as a machine that knows them lists them, to introduce them to another.
    private func card(_ personId: PersonId) -> PersonCard? {
        let known = people.values.flatMap(\.self).first { $0.personId == personId && !$0.passkeys.isEmpty }
        return known.map { PersonCard(name: $0.name, passkeys: $0.passkeys) }
    }

    /// Your passkey's assertion over a fresh challenge from `hostId`.
    private func assertion(on hostId: HostId, sign: PasskeySigner) async throws -> PasskeyAssertion {
        guard case .passkeyChallenge(let challenge, _) = try await send(hostId, .passkeyChallenge) else {
            throw HerderError.Local(detail: "the machine did not send a challenge")
        }
        return try await sign(challenge)
    }
}

/// People as Setup and a machine's page list them: a row per person, each opening their
/// sheet, every change signed with your passkey; then the machines are asked again.
struct PeopleSection: View {
    let fleet: Fleet
    let people: FleetPeople
    let directory: Directory
    /// The machine whose people show, which an invite is to; else the first machine you own.
    var hostId: HostId?
    @Binding var sheet: AppSheet?
    @State private var shown: Directory.Person?
    @State private var error: String?
    @Environment(\.signPasskey) private var sign
    @Environment(\.pages) private var pages

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let error, shown == nil {
                Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("people-error")
            }
            PeopleView(directory: directory, setRole: setRole,
                       show: { person in
                           if person.id == directory.me.id { pages.open(.you) } else { shown = person }
                       },
                       invite: invite)
        }
        .sheet(item: $shown) { person in
            PersonSheetHost(fleet: fleet, people: people, hostId: hostId, personId: person.id, error: error,
                            setRole: setRole, signOut: signOut, itsMe: itsMe)
        }
        .onChange(of: shown) { error = nil }
    }

    /// Gives `person` `role` on `host`, or on every machine for nil: a member on every one
    /// but the vaults, where a member has nothing to do.
    private func setRole(_ person: Directory.Person, _ role: Role?, _ host: HostId?) {
        let machines = fleet.machines
        let hosts = host.map { id in directory.columns.filter { $0.id == id } }
            ?? directory.columns.filter { !$0.vault || role != .member }
        run { try await people.setRole(role, of: person, on: hosts, machines: machines, sign: sign) }
    }

    private func signOut(_ device: Directory.Device) {
        let machines = fleet.machines
        run { try await people.signOut(device, machines: machines, sign: sign) }
    }

    private func itsMe(_ person: Directory.Person) {
        let machines = fleet.machines
        let me = directory.me.id
        run { try await people.itsMe(person, me: me, machines: machines, sign: sign) }
    }

    /// Invites someone to the machine shown, else to every machine you own.
    private func invite() {
        sheet = .invite(hostId)
    }

    private func run(_ action: @escaping () async throws -> Void) {
        error = nil
        Task {
            do { try await action() } catch { self.error = describe(error) }
        }
    }
}

/// A person's sheet over the people it was opened from, as the machines list them now: it
/// follows each change, and has nothing to show once they are gone.
private struct PersonSheetHost: View {
    let fleet: Fleet
    let people: FleetPeople
    let hostId: HostId?
    let personId: String
    let error: String?
    let setRole: (Directory.Person, Role?, HostId?) -> Void
    let signOut: (Directory.Device) -> Void
    let itsMe: (Directory.Person) -> Void

    var body: some View {
        let directory = people.directory(fleet.machines).map { directory in hostId.map(directory.only) ?? directory }
        if let directory, let person = directory.teammates.first(where: { $0.id == personId }) {
            PersonSheet(directory: directory, person: person, error: error,
                        setRole: { setRole(person, $0, $1) }, signOut: signOut, itsMe: { itsMe(person) })
        }
    }
}

/// One machine's people, opened from its page.
struct PeoplePage: View {
    let fleet: Fleet
    let hostId: HostId
    @Binding var sheet: AppSheet?
    @State private var people: FleetPeople

    init(fleet: Fleet, hostId: HostId, sheet: Binding<AppSheet?>) {
        self.fleet = fleet
        self.hostId = hostId
        _sheet = sheet
        _people = State(initialValue: FleetPeople(fleet: fleet))
    }

    var body: some View {
        let machines = fleet.machines
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let directory = people.directory(machines)?.only(hostId) {
                    PeopleSection(fleet: fleet, people: people, directory: directory, hostId: hostId, sheet: $sheet)
                } else if people.failures.isEmpty {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 120)
                } else {
                    Text(people.failures.values.sorted().joined(separator: "\n"))
                        .font(Theme.body).foregroundStyle(Theme.secondary)
                }
            }
            .pageColumn()
        }
        .background(.ground)
        .navigationTitle("People")
        #if os(iOS)
        .toolbar {
            Button("Invite", systemImage: "plus") { sheet = .invite(hostId) }
        }
        #endif
        .refreshable { await people.reload(machines) }
        .task(id: machines.filter { $0.connection == .connected }.map(\.hostId)) { await people.reload(machines) }
        .onChange(of: sheet) { if sheet == nil { Task { await people.reload(machines) } } }
    }

}

/// A machine's way to its people: the People page, narrowed to it.
struct PeopleLink: View {
    let hostId: HostId
    @Environment(\.pages) private var pages

    var body: some View {
        SettingsList(title: "People") {
            SummaryRow(title: "People on this machine", summary: "Their roles here, and who can use it") {
                pages.open(.people(hostId))
            }
        }
        .accessibilityIdentifier("machine-people")
    }
}
