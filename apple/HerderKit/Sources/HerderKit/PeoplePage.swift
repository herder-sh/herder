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

    /// A one-time link that lets someone join a machine with a passkey of their own.
    struct Invite: Equatable {
        let role: Role
        let link: String
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

    /// Mints a link that lets someone join `hostId` as `role`.
    func invite(_ role: Role, on hostId: HostId, sign: PasskeySigner) async throws -> Invite {
        let assertion = try await assertion(on: hostId, sign: sign)
        guard case .invite(let invite, let role, let fingerprint, let addresses, let expiresAt) =
            try await send(hostId, .invite(role: role, assertion: assertion))
        else {
            throw HerderError.Local(detail: "the machine did not mint an invite")
        }
        return Invite(role: role, link: Self.inviteLink(addresses: addresses, fingerprint: fingerprint, invite: invite),
                      expiresAt: Timestamp.date(expiresAt))
    }

    /// `herder://invite?host=…&fp=…&invite=…`.
    nonisolated static func inviteLink(addresses: [String], fingerprint: String, invite: String) -> String {
        var link = URLComponents()
        link.scheme = "herder"
        link.host = "invite"
        link.queryItems = addresses.map { URLQueryItem(name: "host", value: $0) }
            + [URLQueryItem(name: "fp", value: fingerprint), URLQueryItem(name: "invite", value: invite)]
        return link.string ?? ""
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

/// The People page: the whole fleet's, or one machine's when opened from its page.
struct PeoplePage: View {
    let fleet: Fleet
    /// The machine whose people show; every machine's when nil.
    var hostId: HostId?
    @Binding var sheet: AppSheet?
    @State private var people: FleetPeople
    @State private var error: String?
    @Environment(\.signPasskey) private var sign
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    init(fleet: Fleet, hostId: HostId? = nil, sheet: Binding<AppSheet?>) {
        self.fleet = fleet
        self.hostId = hostId
        _sheet = sheet
        _people = State(initialValue: FleetPeople(fleet: fleet))
    }

    var body: some View {
        let machines = fleet.machines
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("people-error")
                }
                if let directory = directory(machines) {
                    PeopleView(directory: directory, grid: grid,
                               setRole: { person, role, host in
                                   let hosts = host.map { id in directory.columns.filter { $0.id == id } }
                                       ?? directory.columns.filter { !$0.vault || role != .member }
                                   run { try await people.setRole(role, of: person, on: hosts, machines: machines, sign: sign) }
                               },
                               revoke: { device in run { try await people.signOut(device, machines: machines, sign: sign) } },
                               itsMe: { person in
                                   run { try await people.itsMe(person, me: directory.me.id, machines: machines, sign: sign) }
                               },
                               invite: invite,
                               claim: fleet.hasUnclaimed ? { fleet.offerClaim(in: $sheet) } : nil)
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
            Button("Invite", systemImage: "plus", action: invite)
        }
        #endif
        .refreshable { await people.reload(machines) }
        .task(id: machines.filter { $0.connection == .connected }.map(\.hostId)) { await people.reload(machines) }
        .onChange(of: sheet) { if sheet == nil { Task { await people.reload(machines) } } }
    }

    private func directory(_ machines: [Machine]) -> Directory? {
        guard let directory = people.directory(machines) else { return nil }
        return hostId.map(directory.only) ?? directory
    }

    private var grid: Bool {
        #if os(iOS)
        sizeClass != .compact
        #else
        true
        #endif
    }

    /// Invites someone to the machine shown, else the first machine you own.
    private func invite() {
        if let host = PeoplePage.inviteHost(fleet.machines, shown: hostId) { sheet = .invite(host) }
    }

    nonisolated static func inviteHost(_ machines: [Machine], shown: HostId?) -> HostId? {
        shown ?? machines.first { $0.role == .owner && $0.connection == .connected && $0.hosts.isEmpty && $0.vault == nil }?.hostId
    }

    private func run(_ action: @escaping () async throws -> Void) {
        error = nil
        Task {
            do { try await action() } catch { self.error = describe(error) }
        }
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
