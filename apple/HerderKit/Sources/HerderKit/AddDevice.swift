import Foundation
import Herder
import Observation
import SwiftUI

/// A link Add a machine takes, scanned, pasted or opened: a request to approve, or the machines
/// another device of yours shares.
enum PairLink: Hashable {
    /// `herder://own` of `herder claim`, or `herder://login` of `herder login`.
    case approve(ApprovalRequest)
    /// `herder://fleet` of Add a Device on another device of yours: its machines, to sign in to
    /// as you.
    case fleet(FleetLink)

    /// Nil for a link of another scheme or kind, or one missing what it needs.
    init?(url: URL) {
        guard url.scheme?.lowercased() == "herder" else { return nil }
        if url.host()?.lowercased() == "fleet" {
            guard let link = try? parseFleetLink(link: url.absoluteString), !link.machines.isEmpty else { return nil }
            self = .fleet(link)
        } else if let request = ApprovalRequest(url: url) {
            self = .approve(request)
        } else {
            return nil
        }
    }

    /// The sheet that takes it: the approval, or signing in to the shared machines.
    var sheet: AppSheet {
        switch self {
        case .approve(let request): .approve(request)
        case .fleet(let link): .join(link)
        }
    }
}

/// The link Add a machine takes in text from a QR code or the clipboard: alone, or somewhere in
/// a copy of the command's output. Nil when there is none.
func pairLink(in text: String) -> PairLink? {
    text.split(whereSeparator: \.isWhitespace)
        .lazy
        .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "'\"<>()`")) }
        .compactMap { URL(string: $0).flatMap(PairLink.init(url:)) }
        .first
}

/// Signing this device in to the machines another device of yours shares, one after the other,
/// each with your passkey: one Face ID per machine, as each asks its own challenge. A machine
/// this device has already is shown as added and left alone. Closing Face ID stops, leaving the
/// rest to try again.
@MainActor @Observable
final class FleetJoin {
    enum State: Equatable {
        /// On this device already, by this name.
        case added(String)
        case waiting
        case signingIn
        /// Signed in, by this name.
        case signedIn(String)
        /// No address of it answered.
        case unreachable
        /// It knows no person by the passkey signed with.
        case unknownPasskey
        case failed(String)

        /// Whether signing in is still to do, or to try again.
        var pending: Bool {
            switch self {
            case .added, .signedIn, .signingIn: false
            case .waiting, .unreachable, .unknownPasskey, .failed: true
            }
        }
    }

    struct Row: Identifiable, Equatable {
        let machine: FleetMachine
        var state: State
        var id: String { machine.fingerprint }

        /// The machine's name on this device once on it, else the one the link gives it, else
        /// its address.
        var name: String {
            switch state {
            case .added(let name), .signedIn(let name): name
            default: FleetJoin.name(machine)
            }
        }
    }

    /// Signs this device in to a machine with your passkey: its name.
    typealias SignIn = @MainActor (FleetMachine) async throws -> String

    private(set) var rows: [Row]
    private(set) var running = false
    /// Whether signing in started at least once.
    private(set) var started = false
    private let signIn: SignIn

    /// `known` names a machine this device has already, by its fingerprint.
    init(_ link: FleetLink, known: (String) -> String?, signIn: @escaping SignIn) {
        rows = link.machines.map { Row(machine: $0, state: known($0.fingerprint).map(State.added) ?? .waiting) }
        self.signIn = signIn
    }

    /// Signs in to each machine still to do, in the link's order.
    func run() async {
        guard !running else { return }
        running = true
        started = true
        defer { running = false }
        for index in rows.indices where rows[index].state.pending {
            rows[index].state = .signingIn
            do {
                rows[index].state = .signedIn(try await signIn(rows[index].machine))
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
        case .Rejected(let info)? where info.code == .forbidden: .unknownPasskey
        default: .failed(describe(error))
        }
    }

    /// The machines this device does not have yet, as the link listed them.
    var toJoin: Int { rows.filter { if case .added = $0.state { false } else { true } }.count }
    var signedIn: Int { rows.filter { if case .signedIn = $0.state { true } else { false } }.count }
    var pending: Int { rows.filter(\.state.pending).count }

    /// "Sign in to 5 machines as you".
    var title: String {
        switch toJoin {
        case 0: "Your machines are on this device already"
        case 1: "Sign in to 1 machine as you"
        default: "Sign in to \(toJoin) machines as you"
        }
    }

    /// The sheet's one line: what Face ID asks, or how it went.
    func summary(biometry: String) -> String {
        guard started else {
            return toJoin > 1 ? "\(biometry) asks once for each machine." : ""
        }
        if running { return "Signed in to \(signedIn) of \(toJoin)…" }
        let failed = rows.filter { $0.state.pending && $0.state != .waiting }.count
        if failed == 0 && pending == 0 { return "Signed in to all \(toJoin)." }
        return "Signed in to \(signedIn) of \(toJoin)." + (failed > 0 ? " \(failed) need\(failed == 1 ? "s" : "") you." : "")
    }
}

extension FleetJoin.State {
    /// What happened, in plain words.
    var title: String {
        switch self {
        case .added: "Already on this device"
        case .waiting: "Waiting"
        case .signingIn: "Signing in…"
        case .signedIn: "Signed in as you"
        case .unreachable: "Can’t reach it"
        case .unknownPasskey: "It doesn’t know this passkey"
        case .failed: "Couldn’t sign in"
        }
    }

    /// What to do about it, where there is something to do.
    var help: String {
        switch self {
        case .added, .waiting, .signingIn, .signedIn: ""
        case .unreachable:
            "Is this device on the same network, or on Tailscale? Try again once it can reach the machine."
        case .unknownPasskey:
            "Pick the herder passkey your other device uses. If you are not on this machine yet, ask its owner to invite you, or run `herder claim` on it."
        case .failed(let why): why
        }
    }
}

extension Fleet {
    /// Signs this device in to `machine` of a shared fleet with your passkey: its name.
    func join(_ machine: FleetMachine, with authenticator: Authenticator) async throws -> String {
        let hostId = try await signIn(hosts: machine.hosts, fingerprint: machine.fingerprint, with: authenticator)
        return client.machines().first { $0.hostId == hostId }?.name ?? FleetJoin.name(machine)
    }

    /// The link that adds another device of yours to every machine this one has: nil while it
    /// has none.
    var shareLink: String? { client.share().map(fleetLinkToString) }
}

extension FleetJoin {
    /// "10.0.4.17": where a machine is.
    nonisolated static func address(_ machine: FleetMachine) -> String {
        ApprovalRequest.address(machine.hosts.first ?? "")
    }

    /// "devbox": the name the sharing device gives a machine, else its address.
    nonisolated static func name(_ machine: FleetMachine) -> String {
        machine.name.flatMap { $0.isEmpty ? nil : $0 } ?? address(machine)
    }
}

/// Add a Device: the link of every machine this device has, as a QR code for your other device
/// to scan, with the steps.
struct AddDeviceSheet: View {
    let link: String?
    @Environment(\.dismiss) private var dismiss

    static let subtitle = "Use your machines on your iPhone, iPad or another Mac too."

    var body: some View {
        SheetScaffold(title: "Add a Device", subtitle: Self.subtitle, height: 820) {
            AddDeviceSteps(link: link)
        } footer: {
            Spacer()
            ActionButton(title: "Done", style: .primary) { dismiss() }
                .frame(maxWidth: 160)
                .keyboardShortcut(.defaultAction)
        }
    }
}

/// Add a Device's steps, around the QR code of `link`.
struct AddDeviceSteps: View {
    let link: String?

    var body: some View {
        if let link {
            NumberedStep(number: 1, title: "Install herder on the other device") {
                note(Text("Then open it."))
            }
            NumberedStep(number: 2, title: "Scan this code with it") {
                note(Text("Choose \(Text("I have herder on another device").bold()), then scan this code. On a Mac, copy the link instead: Add a Machine picks it up."))
                InviteLinkCode(link: link, expiresAt: nil,
                               hint: "It lists where your machines are and how to recognise them (their fingerprints).")
            }
            NumberedStep(number: 3, title: "Approve with your passkey") {
                note(Text("The same passkey this device uses. It reaches your other devices through iCloud Keychain or a password manager such as 1Password."))
            }
            Label {
                Text("The code grants nothing by itself: each machine lets in only a device that has your passkey.")
            } icon: {
                Image(systemName: "lock")
            }
            .font(.footnote)
            .foregroundStyle(Theme.secondary)
            .fixedSize(horizontal: false, vertical: true)
        } else {
            note(Text("This device has no machines yet to share. Add one first."))
        }
    }

    private func note(_ text: Text) -> some View {
        text.font(.footnote).foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
    }
}

/// Signing in to the machines of a `herder://fleet` link, with your passkey.
struct JoinFleetSheet: View {
    @State private var join: FleetJoin

    init(fleet: Fleet, link: FleetLink, authenticator: Authenticator = passkeyAuthenticator()) {
        _join = State(initialValue: FleetJoin(link, known: { fleet.machine(fingerprint: $0)?.name }) {
            try await fleet.join($0, with: authenticator)
        })
    }

    var body: some View {
        JoinFleetView(join: join)
    }
}

/// The machines of a shared fleet, each with how signing in to it went, and the one button
/// that signs in to those still to do.
struct JoinFleetView: View {
    let join: FleetJoin
    var biometry = Biometry.name

    static let subtitle = "Another device of yours shared these. Each one lets you in with your passkey."

    var body: some View {
        SheetScaffold(title: join.title, subtitle: Self.subtitle, height: 640, hint: join.summary(biometry: biometry)) {
            JoinFleetRows(join: join)
        } footer: {
            Spacer()
            JoinFleetButtons(join: join, biometry: biometry)
        }
    }
}

/// The shared fleet's machines, one row each.
struct JoinFleetRows: View {
    let join: FleetJoin

    var body: some View {
        SettingsGroup {
            ForEach(Array(join.rows.enumerated()), id: \.element.id) { index, row in
                if index > 0 { RowDivider() }
                JoinRow(row: row)
            }
        }
        Text("Before you sign in, each machine proves it is the one your other device knows (by its fingerprint).")
            .font(.footnote).foregroundStyle(Theme.tertiary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// Sign in, or try again what failed; Done once nothing is left, or to leave it.
struct JoinFleetButtons: View {
    let join: FleetJoin
    let biometry: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        if join.pending > 0 {
            if join.started {
                ActionButton(title: "Done", style: .secondary) { dismiss() }
                    .frame(maxWidth: 120)
                    .disabled(join.running)
            }
            ActionButton(title: join.started ? "Try Again" : "Sign In with \(biometry)", style: .primary) {
                await join.run()
            }
            .frame(maxWidth: 220)
            .disabled(join.running)
            .keyboardShortcut(.defaultAction)
            .accessibilityIdentifier("join-fleet")
        } else {
            ActionButton(title: "Done", style: .primary) { dismiss() }
                .frame(maxWidth: 160)
                .disabled(join.running)
                .keyboardShortcut(.defaultAction)
        }
    }
}

/// A machine of a shared fleet: its name, its address beside it, and how it went.
private struct JoinRow: View {
    let row: FleetJoin.Row

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            icon.frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                Text(row.state.title).font(.caption).foregroundStyle(tint)
                if !row.state.help.isEmpty {
                    Text(LocalizedStringKey(row.state.help)).font(.caption).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            if name != address {
                Text(address).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("join-\(row.id)")
    }

    private var address: String { FleetJoin.address(row.machine) }
    private var name: String { row.name }

    @ViewBuilder private var icon: some View {
        switch row.state {
        case .signingIn: ProgressView().controlSize(.small)
        default: Image(systemName: symbol).font(.body).foregroundStyle(tint)
        }
    }

    private var symbol: String {
        switch row.state {
        case .added: "checkmark.circle"
        case .waiting, .signingIn: "circle"
        case .signedIn: "checkmark.circle.fill"
        case .unreachable: "wifi.exclamationmark"
        case .unknownPasskey: "person.badge.key"
        case .failed: "exclamationmark.triangle.fill"
        }
    }

    private var tint: Color {
        switch row.state {
        case .added, .waiting, .signingIn: Theme.tertiary
        case .signedIn: Theme.success
        case .unreachable: Theme.accent
        case .unknownPasskey, .failed: Theme.failure
        }
    }
}
