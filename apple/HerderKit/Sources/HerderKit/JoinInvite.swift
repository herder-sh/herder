import Foundation
import Herder
import Observation
import SwiftUI

/// Joining the machines of a teammate's invite: the first machine that answers makes your
/// passkey and takes you with it, and each other machine takes you by that passkey, one Face ID
/// each. When this device knows you already, every machine takes your passkey instead of a new
/// one, so you stay one person. A machine this device has already is left alone.
@MainActor @Observable
final class InviteJoin {
    enum State: Equatable {
        /// On this device already, by this name.
        case added(String)
        case waiting
        case joining
        /// Joined, by this name.
        case joined(String)
        /// No address of it answered.
        case unreachable
        /// It refused the invite: used, or expired.
        case refused
        case failed(String)

        /// Whether joining is still to do, or to try again.
        var pending: Bool {
            switch self {
            case .added, .joined, .joining: false
            case .waiting, .unreachable, .refused, .failed: true
            }
        }
    }

    struct Row: Identifiable, Equatable {
        let machine: InviteLink.Machine
        var state: State
        var id: String { machine.id }

        /// Its name on this device once on it, else the one the invite gives it.
        var name: String {
            switch state {
            case .added(let name), .joined(let name): name
            default: machine.title
            }
        }
    }

    /// Makes a passkey named `name` from `machine`'s offer, and joins it with that: the
    /// machine's name, and you as it lists you, with the new passkey.
    typealias Register = @MainActor (InviteLink.Machine, String) async throws -> (name: String, me: PersonInfo)
    /// Joins `machine` as `me`, by a passkey of theirs: the machine's name.
    typealias Accept = @MainActor (InviteLink.Machine, PersonInfo) async throws -> String

    let link: InviteLink
    private(set) var rows: [Row]
    /// Your name, for a new passkey.
    var name: String
    /// You, once a machine knows your passkey, or as this device knows you already.
    var me: PersonInfo?
    private(set) var running = false
    private(set) var started = false
    private let register: Register
    private let accept: Accept

    /// `known` names a machine this device has already, by its fingerprint.
    init(_ link: InviteLink, known: (String) -> String?, me: PersonInfo? = nil,
         register: @escaping Register, accept: @escaping Accept) {
        self.link = link
        rows = link.machines.map { Row(machine: $0, state: known($0.fingerprint).map(State.added) ?? .waiting) }
        name = link.label ?? ""
        self.me = me
        self.register = register
        self.accept = accept
    }

    /// Joins each machine still to do, in the link's order. Until one takes the new passkey,
    /// each is asked to make it; closing Face ID stops, leaving the rest to try again.
    func run() async {
        guard !running else { return }
        running = true
        started = true
        defer { running = false }
        let name = name.trimmingCharacters(in: .whitespaces)
        for index in rows.indices where rows[index].state.pending {
            rows[index].state = .joining
            do {
                if let me {
                    rows[index].state = .joined(try await accept(rows[index].machine, me))
                } else {
                    let (machine, me) = try await register(rows[index].machine, name)
                    self.me = me
                    rows[index].state = .joined(machine)
                }
            } catch where isCancelled(error) {
                rows[index].state = .waiting
                return
            } catch {
                rows[index].state = Self.state(of: error)
            }
        }
    }

    nonisolated static func state(of error: Error) -> State {
        switch error as? HerderError {
        case .Unreachable?: .unreachable
        case .Rejected(let info)? where info.code == .forbidden: .refused
        default: .failed(describe(error))
        }
    }

    /// The machines this device does not have yet.
    var toJoin: Int { rows.filter { if case .added = $0.state { false } else { true } }.count }
    var joined: Int { rows.filter { if case .joined = $0.state { true } else { false } }.count }
    var pending: Int { rows.filter(\.state.pending).count }
    /// Whether it makes a new passkey, and so asks your name.
    var newPasskey: Bool { me == nil }

    /// "Tomas invited you to herder".
    var title: String {
        guard started else { return "\(link.by ?? "A teammate") invited you to herder" }
        if running { return "Joining…" }
        if pending == 0 { return toJoin == 0 ? "You’re on these machines already" : "You’re in" }
        return "You’re on \(joined) of \(toJoin) machine\(toJoin == 1 ? "" : "s")"
    }

    /// "As a member on 5 machines."
    var subtitle: String { "As \(link.grant)." }

    /// The primary button: what it does, by Face ID.
    func action(biometry: String) -> String {
        if started { return "Try Again" }
        return newPasskey ? "Create Passkey and Join" : "Join with \(biometry)"
    }

    /// The one line beside the button.
    func hint(biometry: String) -> String {
        guard !started else { return "" }
        return toJoin > 1 ? "\(biometry) asks once for each machine." : ""
    }
}

extension InviteJoin.State {
    /// What happened, in plain words.
    var title: String {
        switch self {
        case .added: "Already on this device"
        case .waiting: "Waiting"
        case .joining: "Joining…"
        case .joined: "Joined"
        case .unreachable: "Can’t reach it"
        case .refused: "The invite was used or has expired"
        case .failed: "Couldn’t join"
        }
    }

    /// What to do about it, where there is something to do; `by` is who invited you.
    func help(by: String?) -> String {
        switch self {
        case .added, .waiting, .joining, .joined: ""
        case .unreachable:
            "Is this device on the same network, or on Tailscale? Try again once it can reach the machine."
        case .refused: "Ask \(by ?? "whoever invited you") for a new link."
        case .failed(let why): why
        }
    }
}

extension Fleet {
    /// Makes a passkey named `name` from `machine`'s offer, and joins it with the invite.
    func acceptInvite(_ machine: InviteLink.Machine, name: String,
                      with authenticator: Authenticator) async throws -> (name: String, me: PersonInfo) {
        let pending = try await client.signIn(hosts: machine.hosts, fingerprint: machine.fingerprint, passkeys: [])
        guard let creation = pending.registration(),
              let userId = Data(base64URL: creation.userHandle), let challenge = Data(base64URL: creation.challenge)
        else { throw HerderError.Local(detail: "the machine did not offer to make a passkey") }
        let registration = try await authenticator.register(name: name, userId: userId, challenge: challenge)
        let person = NewPerson(name: name, passkey: .register(registration: registration))
        let hostId = try await pending.finish(signIn: .acceptInvite(invite: machine.invite, person: person)).hostId
        // The machine lists the new passkey's public key, which the others take you by.
        guard case .people(let people) = try await client.send(hostId: hostId, command: .listPeople),
              let me = people.first(where: { $0.passkeys.contains { $0.credentialId == registration.id } })
        else { throw HerderError.Local(detail: "the machine did not list your new passkey") }
        return (client.machines().first { $0.hostId == hostId }?.name ?? machine.title, me)
    }

    /// Joins `machine` with the invite as `me`, by a passkey of theirs.
    func acceptInvite(_ machine: InviteLink.Machine, as me: PersonInfo,
                      with authenticator: Authenticator) async throws -> String {
        let pending = try await client.signIn(hosts: machine.hosts, fingerprint: machine.fingerprint, passkeys: [])
        let assertion = try await authenticator.sign(pending.challenge())
        guard let key = me.passkeys.first(where: { $0.credentialId == assertion.id }) else {
            throw HerderError.Local(detail: "that passkey is not \(me.name)’s; sign with the one you joined with")
        }
        let person = NewPerson(name: me.name,
                               passkey: .existing(personId: me.personId, key: key, assertion: assertion))
        let hostId = try await pending.finish(signIn: .acceptInvite(invite: machine.invite, person: person)).hostId
        return client.machines().first { $0.hostId == hostId }?.name ?? machine.title
    }

    /// You, as a machine this device is on lists you with your passkeys: nil on a device that
    /// knows nobody yet.
    func knownPerson() async -> PersonInfo? {
        guard let me = FleetPeople.me(machines) else { return nil }
        let people = FleetPeople(fleet: self)
        await people.reload(machines)
        return Self.keyed(me, in: people.people)
    }
}

/// Joining from a teammate's `herder://invite` link.
struct JoinInviteSheet: View {
    let fleet: Fleet
    @State private var join: InviteJoin

    init(fleet: Fleet, link: InviteLink, authenticator: Authenticator = passkeyAuthenticator()) {
        self.fleet = fleet
        _join = State(initialValue: InviteJoin(
            link, known: { fleet.machine(fingerprint: $0)?.name },
            register: { try await fleet.acceptInvite($0, name: $1, with: authenticator) },
            accept: { try await fleet.acceptInvite($0, as: $1, with: authenticator) }))
    }

    var body: some View {
        JoinInviteView(join: join)
            .task {
                guard join.me == nil, !join.started, let me = await fleet.knownPerson() else { return }
                join.me = me
            }
    }
}

/// Who invited you and to what, your name for the new passkey, then each machine with how
/// joining it went.
struct JoinInviteView: View {
    let join: InviteJoin
    var biometry = Biometry.name

    var body: some View {
        SheetScaffold(title: join.started ? join.title : "Join herder", height: 680, hint: join.hint(biometry: biometry)) {
            JoinInviteContent(join: join)
        } footer: {
            Spacer()
            JoinInviteButtons(join: join, biometry: biometry)
        }
    }
}

/// Before joining, who invited you to what and your name for the passkey; then, and after,
/// each machine with how joining it went.
struct JoinInviteContent: View {
    @Bindable var join: InviteJoin
    /// Where the new passkey is kept.
    var keychain = "Apple Passwords"

    var body: some View {
        if !join.started {
            VStack(spacing: 10) {
                Avatar(name: join.link.by ?? "?", id: join.link.by ?? "", size: 64)
                Text(join.title).font(.title2.weight(.bold)).foregroundStyle(Theme.text)
                    .multilineTextAlignment(.center)
                Text(join.subtitle).font(.subheadline).foregroundStyle(Theme.secondary)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            if join.newPasskey {
                Field(label: "Your name") {
                    InputBox(placeholder: "Lina", text: $join.name)
                        .accessibilityIdentifier("invite-join-name")
                }
                Label {
                    Text(passkeyNote)
                } icon: {
                    Image(systemName: "person.badge.key")
                }
                .font(.footnote).foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
            } else if let me = join.me {
                Label("You join as \(me.name), with the passkey this device uses.", systemImage: "person.badge.key")
                    .font(.footnote).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            SettingsList(title: "Machines") { rows }
        } else {
            SettingsGroup { rows }
        }
    }

    private var rows: some View {
        ForEach(Array(join.rows.enumerated()), id: \.element.id) { index, row in
            if index > 0 { RowDivider() }
            InviteJoinRow(row: row, role: join.link.role, by: join.link.by)
        }
    }

    private var passkeyNote: String {
        let name = join.name.trimmingCharacters(in: .whitespaces)
        let named = name.isEmpty ? "a passkey" : "a passkey named “\(name)”"
        return "herder creates \(named) in \(keychain). You’ll use it on every machine."
    }
}

/// Create Passkey and Join, Try Again what failed, or Start Using herder once nothing is left.
struct JoinInviteButtons: View {
    let join: InviteJoin
    let biometry: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        if join.pending > 0 {
            if join.started {
                ActionButton(title: "Later", style: .secondary) { dismiss() }
                    .frame(maxWidth: 120)
                    .disabled(join.running)
            }
            ActionButton(title: join.action(biometry: biometry), style: .primary) { await join.run() }
                .frame(maxWidth: 260)
                .disabled(join.running || (join.newPasskey && join.name.trimmingCharacters(in: .whitespaces).isEmpty))
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("invite-join")
        } else {
            ActionButton(title: "Start Using herder", style: .primary) { dismiss() }
                .frame(maxWidth: 220)
                .keyboardShortcut(.defaultAction)
        }
    }
}

/// A machine of the invite: its name, its address beside it, and how joining it went.
private struct InviteJoinRow: View {
    let row: InviteJoin.Row
    let role: Role
    let by: String?

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            icon.frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                Text(status).font(.caption).foregroundStyle(tint)
                let help = row.state.help(by: by)
                if !help.isEmpty {
                    Text(help).font(.caption).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            if row.name != address {
                Text(address).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("invite-join-\(row.id)")
    }

    private var address: String { ApprovalRequest.address(row.machine.hosts.first ?? "") }

    /// "Member", once joined.
    private var status: String {
        if case .joined = row.state { return role == .owner ? "Owner" : "Member" }
        return row.state.title
    }

    @ViewBuilder private var icon: some View {
        switch row.state {
        case .joining: ProgressView().controlSize(.small)
        default: Image(systemName: symbol).font(.body).foregroundStyle(tint)
        }
    }

    private var symbol: String {
        switch row.state {
        case .added, .joined: "checkmark.circle.fill"
        case .waiting, .joining: "circle.dashed"
        case .unreachable, .refused, .failed: "exclamationmark.circle.fill"
        }
    }

    private var tint: Color {
        switch row.state {
        case .added, .waiting, .joining: Theme.tertiary
        case .joined: Theme.success
        case .unreachable, .refused, .failed: Theme.accent
        }
    }
}
