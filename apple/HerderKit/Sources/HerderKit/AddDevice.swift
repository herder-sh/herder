import Foundation
import Herder
import Observation
import SwiftUI
#if os(iOS)
import UIKit
#endif

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

/// Signing this device in to the machines another device of yours shares. Each machine is
/// reached first, before any Face ID, so one that cannot be reached says why while the rest
/// sign in. Then one button signs in to every machine reached, one after the other, each with
/// your passkey: Face ID asks once per machine, as each asks its own challenge. A machine this
/// device has already is shown as added and left alone. Closing Face ID stops, leaving the rest
/// to sign in again.
@MainActor @Observable
final class FleetJoin {
    enum State: Equatable {
        /// On this device already, by this name.
        case added(String)
        /// Being reached, before signing in.
        case checking
        /// Reached: signing in is next.
        case ready
        /// No address of it answered.
        case unreachable
        case signingIn
        /// Signed in, by this name.
        case signedIn(String)
        /// It knows you by another passkey: which, and where it is kept ("Tomas in 1Password").
        case otherPasskey(String)
        /// It knows nobody by the passkey signed with: its owners, to ask.
        case unknownPerson(owners: [String])
        /// The passkey signed without Face ID, which every machine needs.
        case notVerified
        case failed(String)

        /// Whether signing in is still to do, or to try again.
        var pending: Bool {
            switch self {
            case .added, .signedIn, .signingIn: false
            case .checking, .ready, .unreachable, .otherPasskey, .unknownPerson, .notVerified, .failed: true
            }
        }

        /// Whether it waits on you: a cause shown with its fix.
        var needsYou: Bool {
            switch self {
            case .unreachable, .otherPasskey, .unknownPerson, .notVerified, .failed: true
            case .added, .checking, .ready, .signingIn, .signedIn: false
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

    /// Reaches a machine, without signing in: throws `Unreachable` when no address answers.
    typealias Check = @MainActor (FleetMachine) async throws -> Void
    /// Signs this device in to a machine with your passkey: its name.
    typealias SignIn = @MainActor (FleetMachine) async throws -> String

    /// Who shared the machines, and the passkeys of theirs that sign in.
    let person: FleetPerson
    private(set) var rows: [Row]
    private(set) var running = false
    /// Whether signing in started at least once.
    private(set) var started = false
    private let reach: Check
    private let signer: SignIn

    /// `known` names a machine this device has already, by its fingerprint. Every other
    /// machine starts out being reached (`check()`).
    init(_ link: FleetLink, known: (String) -> String?, check: @escaping Check, signIn: @escaping SignIn) {
        person = link.person
        rows = link.machines.map { Row(machine: $0, state: known($0.fingerprint).map(State.added) ?? .checking) }
        reach = check
        signer = signIn
    }

    /// Reaches every machine still to reach, all at once.
    func check() async {
        await withTaskGroup(of: Void.self) { group in
            for index in rows.indices where rows[index].state == .checking {
                group.addTask { await self.check(at: index) }
            }
        }
    }

    private func check(at index: Int) async {
        rows[index].state = .checking
        do {
            try await reach(rows[index].machine)
            rows[index].state = .ready
        } catch {
            rows[index].state = Self.state(of: error)
        }
    }

    /// Signs in to each machine reached, in the link's order.
    func run() async {
        guard !running else { return }
        running = true
        started = true
        defer { running = false }
        for index in rows.indices where rows[index].state == .ready {
            guard await signIn(at: index) else { return }
        }
    }

    /// Tries one machine that needs you again: reaches it, then, once signing in has started,
    /// signs in to it.
    func retry(_ id: Row.ID) async {
        guard !running, let index = rows.firstIndex(where: { $0.id == id }), rows[index].state.needsYou else { return }
        running = true
        defer { running = false }
        await check(at: index)
        if started, rows[index].state == .ready { _ = await signIn(at: index) }
    }

    /// False when Face ID was closed, which stops signing in.
    private func signIn(at index: Int) async -> Bool {
        rows[index].state = .signingIn
        do {
            rows[index].state = .signedIn(try await signer(rows[index].machine))
        } catch where isCancelled(error) {
            rows[index].state = .ready
            return false
        } catch {
            rows[index].state = Self.state(of: error)
        }
        return true
    }

    nonisolated static func state(of error: Error) -> State {
        switch error as? HerderError {
        case .Unreachable?: return .unreachable
        case .SignInRefused(let refusal, _)?:
            switch refusal {
            case .otherPasskey(let passkeys): return .otherPasskey(passkeyPlaces(passkeys))
            case .unknownPerson(let owners): return .unknownPerson(owners: owners)
            case .userNotVerified: return .notVerified
            }
        default: return .failed(describe(error))
        }
    }

    /// "Tomas in 1Password": the passkeys a machine knows you by, by where they are kept when
    /// this app knows the place, else by their labels.
    nonisolated static func passkeyPlaces(_ passkeys: [RefusedPasskey]) -> String {
        let kept = passkeys.compactMap { passkey in
            passkey.aaguid.flatMap { passkeyStore(aaguid: $0) }.map { "\(passkey.label) in \($0)" }
        }
        let places = unique(kept.isEmpty ? passkeys.map(\.label) : kept)
        return places.isEmpty ? "another passkey" : places.joined(separator: " or ")
    }

    private nonisolated static func unique(_ items: [String]) -> [String] {
        items.reduce(into: []) { if !$0.contains($1) { $0.append($1) } }
    }

    /// The machines this device does not have yet, as the link listed them.
    var toJoin: Int { rows.filter { if case .added = $0.state { false } else { true } }.count }
    var signedIn: Int { rows.filter { if case .signedIn = $0.state { true } else { false } }.count }
    var pending: Int { rows.filter(\.state.pending).count }
    var ready: Int { rows.filter { $0.state == .ready }.count }
    /// The machines that need you, by name.
    var left: [String] { rows.filter(\.state.needsYou).map(\.name) }

    /// Where the sheet stands, for `FlowProgress`: Sign in (1) while signing in is to do, past
    /// Done (3) once nothing is left.
    var step: Int { pending == 0 ? 3 : 1 }

    /// "Sign in to your 5 machines", then how it went.
    func title(device: String) -> String {
        if toJoin == 0 { return "Your machines are on this \(device) already" }
        if started && !running && pending == 0 {
            return rows.count == 1 ? "This \(device) is on your machine" : "This \(device) is on all \(rows.count) machines"
        }
        if started && !running && ready == 0 { return "Signed in to \(signedIn) of \(toJoin)" }
        return toJoin == 1 ? "Sign in to your machine" : "Sign in to your \(toJoin) machines"
    }

    /// The line under the title: who shared these, or what is left.
    var subtitle: String {
        guard started, !running, ready == 0 else {
            return "Another device of yours shared these. Each one lets you in with your passkey."
        }
        let needs = left.count
        if needs == 0 { return "Signed in as you." }
        return "\(needs == 1 ? "One needs" : "\(needs.spelled) need") you. Fix \(needs == 1 ? "it" : "them") here, or come back to \(needs == 1 ? "it" : "them") later."
    }

    /// The footer's one line: what Face ID asks, or what is left to do.
    func hint(biometry: String) -> String {
        if running { return "Signed in to \(signedIn) of \(toJoin)…" }
        if rows.contains(where: { $0.state == .checking }) { return "Reaching each machine first…" }
        if ready > 0 {
            let asks = ready > 1 ? "\(biometry) asks once for each machine it can reach." : ""
            let wait = left.isEmpty ? "" : " \(Self.list(left)) can wait."
            return (asks + wait).trimmingCharacters(in: .whitespaces)
        }
        if started && !left.isEmpty { return "Left to do: \(Self.list(left)). Scan the code again to finish." }
        return ""
    }

    /// "a", "a and b", "a, b and c".
    nonisolated static func list(_ names: [String]) -> String {
        guard names.count > 1 else { return names.first ?? "" }
        return names.dropLast().joined(separator: ", ") + " and " + (names.last ?? "")
    }
}

private extension Int {
    /// "Two" for 2, up to nine; digits beyond.
    var spelled: String {
        let words = ["Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine"]
        return words.indices.contains(self) ? words[self] : "\(self)"
    }
}

extension FleetJoin.Row {
    /// What happened, in plain words: the cause, for one that needs you. `device` is "iPhone",
    /// `biometry` "Face ID".
    func status(device: String, biometry: String) -> String {
        switch state {
        case .added: "Already on this \(device)"
        case .checking: "Checking…"
        case .ready: "Ready"
        case .signingIn: "Signing in…"
        case .signedIn: "Signed in"
        case .unreachable:
            FleetJoin.onTailscale(machine)
                ? "Can’t reach it: Tailscale may be off on this \(device)"
                : "Can’t reach it: this \(device) may be on another network"
        case .otherPasskey(let places): "Knows you by another passkey: \(places)"
        case .unknownPerson(let owners): owners.first.map { "Ask \($0) to add you" } ?? "Ask its owner to add you"
        case .notVerified: "\(biometry) was skipped, try again"
        case .failed(let why): "Couldn’t sign in: \(why)"
        }
    }

    /// What to do about it, where the button alone does not say.
    func fix(biometry: String) -> String {
        switch state {
        case .unreachable:
            FleetJoin.onTailscale(machine)
                ? "Turn Tailscale on, then try again."
                : "Join the network it is on, or turn on Tailscale, then try again."
        case .otherPasskey:
            "Try again and pick that passkey. Then add this one from it, if you want both."
        case .unknownPerson(let owners):
            owners.isEmpty
                ? "You are not on this machine yet. Ask its owner to invite you, or run `herder claim` on it."
                : "You are not on this machine yet. Owners: \(ListFormatter.localizedString(byJoining: owners))."
        case .notVerified:
            "Your password manager signed without \(biometry), and machines need it. Try again and confirm with \(biometry)."
        case .added, .checking, .ready, .signingIn, .signedIn, .failed: ""
        }
    }
}

/// The password manager a passkey lives in, by the AAGUID of the authenticator that made it;
/// nil for one this app does not know.
func passkeyStore(aaguid: String) -> String? {
    switch aaguid.lowercased() {
    case "fbfc3007-154e-4ecc-8c0b-6e020557d7bd", "dd4ec289-e01d-41c9-bb89-70fa845d4bf2": "Apple Passwords"
    case "bada5566-a7aa-401f-bd96-45619a55120d": "1Password"
    case "ea9b8d66-4d01-1d21-3ce4-b6b48cb575d4": "Google Password Manager"
    case "d548826e-79b4-db40-a3d8-11116f7e8349": "Bitwarden"
    case "531126d6-e717-415c-9320-3d9aa6981239": "Dashlane"
    default: nil
    }
}

extension Fleet {
    /// Reaches `machine` of a shared fleet without signing in: throws `Unreachable` when no
    /// address answers. The connection opened to check closes at once.
    func reach(_ machine: FleetMachine) async throws {
        _ = try await client.signIn(hosts: machine.hosts, fingerprint: machine.fingerprint, passkeys: [])
    }

    /// Signs this device in to `machine` of a shared fleet with one of `person`'s passkeys: its
    /// name.
    func join(_ machine: FleetMachine, as person: FleetPerson, with authenticator: Authenticator) async throws -> String {
        let hostId = try await signIn(hosts: machine.hosts, fingerprint: machine.fingerprint, passkeys: person.passkeys,
                                      with: authenticator)
        return client.machines().first { $0.hostId == hostId }?.name ?? FleetJoin.name(machine)
    }

    /// Whether this device may share its machines: one of them knows it as a person.
    var canShare: Bool { client.machines().contains { $0.personId != nil } }

    /// The link that adds another device of yours to every machine this one has, naming you
    /// and your passkeys, as your connected machines list them.
    func shareLink() async throws -> String {
        let link = try await client.share()
        return fleetLinkToString(link: link)
    }
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

    /// Whether a machine is reached over Tailscale: an address in its 100.64.0.0/10 or
    /// fd7a:115c:a1e0::/48 range, or a MagicDNS name.
    nonisolated static func onTailscale(_ machine: FleetMachine) -> Bool {
        machine.hosts.contains { host in
            let address = ApprovalRequest.address(host).lowercased()
            if address.hasSuffix(".ts.net") || address.hasPrefix("fd7a:115c:a1e0:") { return true }
            let octets = address.split(separator: ".").compactMap { UInt8($0) }
            return octets.count == 4 && octets[0] == 100 && (64...127).contains(octets[1])
        }
    }

    /// "iPhone": what this device is, as the sheet names it.
    @MainActor static var device: String {
        #if os(macOS)
        "Mac"
        #else
        UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone"
        #endif
    }
}

/// Add a device: the `herder://fleet` link of every machine this device has, as a large QR
/// code for your other device to scan, with the machines it shares by name. The link is read
/// from your connected machines as the sheet opens.
struct AddDeviceSheet: View {
    let fleet: Fleet
    @State private var link: Result<FleetLink, Error>?

    static let title = "Add a device"
    static let subtitle = "Scan this with your other device’s camera, or in herder there with Scan Its Code."
    static let hint = "On a Mac, copy the link and paste it in herder."

    var body: some View {
        SheetScaffold(title: Self.title, subtitle: Self.subtitle, height: 760, hint: shared == nil ? "" : Self.hint) {
            switch link {
            case .success(let link)?: AddDeviceContent(link: link)
            case .failure(let error)?:
                Text(describe(error)).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
            case nil: ProgressView().frame(maxWidth: .infinity, minHeight: 120)
            }
        } footer: {
            AddDeviceButtons(link: shared)
        }
        .task {
            do { link = .success(try await fleet.client.share()) } catch { link = .failure(error) }
        }
    }

    private var shared: FleetLink? {
        if case .success(let link)? = link { link } else { nil }
    }
}

/// Add a device's code, the machines it shares, and what it grants.
struct AddDeviceContent: View {
    let link: FleetLink

    var body: some View {
        if let code = QRCode.image(fleetLinkToString(link: link)) {
            Image(decorative: code, scale: 1)
                .interpolation(.none)
                .resizable()
                .frame(width: 240, height: 240)
                .padding(16)
                .background(.white, in: .rect(cornerRadius: 16))
                .frame(maxWidth: .infinity)
                .accessibilityLabel("QR code of the link")
        }
        VStack(alignment: .leading, spacing: 10) {
            SectionHeading(title: link.machines.count == 1 ? "Shares your machine" : "Shares your \(link.machines.count) machines")
            FlowLayout(spacing: 6) {
                ForEach(link.machines, id: \.fingerprint) { machine in
                    Text(FleetJoin.name(machine))
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Theme.text)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .background(Theme.raised, in: .capsule)
                }
            }
        }
        Label {
            Text("The code grants nothing by itself: it holds names and addresses, not keys. Each machine lets in only a device with your passkey.")
        } icon: {
            Image(systemName: "checkmark.shield")
        }
        .font(.footnote)
        .foregroundStyle(Theme.secondary)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// Copy Link, then Done.
struct AddDeviceButtons: View {
    let link: FleetLink?
    @State private var copied = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Spacer()
        if let link {
            ActionButton(title: copied ? "Copied" : "Copy Link", style: .secondary) {
                Clipboard.string = fleetLinkToString(link: link)
                copied = true
            }
            .frame(maxWidth: 140)
            .task(id: copied) {
                guard copied else { return }
                try? await Task.sleep(for: .seconds(1.5))
                copied = false
            }
        }
        ForwardButton(title: "Done") { dismiss() }
            .frame(maxWidth: 120)
            .keyboardShortcut(.defaultAction)
    }
}

/// Signing in to the machines of a `herder://fleet` link, with your passkey.
struct JoinFleetSheet: View {
    @State private var join: FleetJoin

    init(fleet: Fleet, link: FleetLink, authenticator: Authenticator = passkeyAuthenticator()) {
        _join = State(initialValue: FleetJoin(link, known: { fleet.machine(fingerprint: $0)?.name },
                                              check: { try await fleet.reach($0) },
                                              signIn: { try await fleet.join($0, as: link.person, with: authenticator) }))
    }

    var body: some View {
        JoinFleetView(join: join)
            .task { await join.check() }
    }
}

/// Signing in to a shared fleet: where it stands (Scan · Sign in · Done), the passkey it signs
/// in with, each machine with how it went and its fix, and the one button that signs in.
struct JoinFleetView: View {
    let join: FleetJoin
    var biometry = Biometry.name
    var device = FleetJoin.device

    var body: some View {
        SheetScaffold(title: join.title(device: device), subtitle: join.subtitle, height: 760,
                      hint: join.hint(biometry: biometry)) {
            JoinFleetContent(join: join, device: device, biometry: biometry)
        } footer: {
            JoinFleetButtons(join: join, biometry: biometry)
        }
    }
}

/// The sheet's body: progress, the passkey, the machines.
struct JoinFleetContent: View {
    let join: FleetJoin
    let device: String
    let biometry: String

    var body: some View {
        FlowProgress(steps: ["Scan", "Sign in", "Done"], current: join.step)
        if join.toJoin > 0 {
            VStack(alignment: .leading, spacing: 8) {
                SectionHeading(title: "As")
                PasskeyCard(person: join.person, biometry: biometry)
            }
        }
        SettingsGroup {
            ForEach(Array(join.rows.enumerated()), id: \.element.id) { index, row in
                if index > 0 { RowDivider() }
                JoinRow(row: row, device: device, biometry: biometry) { await join.retry(row.id) }
                    .disabled(join.running)
            }
        }
    }
}

/// Sign In with Face ID while machines are reached and waiting; Done for Now while some need
/// you; Done once nothing is left.
struct JoinFleetButtons: View {
    let join: FleetJoin
    let biometry: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Spacer()
        if join.ready > 0 || join.running || !join.started && join.pending > 0 {
            if join.started && !join.running {
                ActionButton(title: "Done for Now", style: .secondary) { dismiss() }
                    .frame(maxWidth: 140)
            }
            ForwardButton(title: "Sign In with \(biometry)") { await join.run() }
                .frame(maxWidth: 240)
                .disabled(join.running || join.ready == 0)
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("join-fleet")
        } else if join.pending > 0 {
            ForwardButton(title: "Done for Now") { dismiss() }
                .frame(maxWidth: 160)
                .keyboardShortcut(.defaultAction)
        } else {
            ForwardButton(title: "Done") { dismiss() }
                .frame(maxWidth: 120)
                .keyboardShortcut(.defaultAction)
        }
    }
}

/// A flow's one forward step: the action colour, filled. Runs `action` once at a time.
private struct ForwardButton: View {
    let title: String
    let action: () async -> Void
    @State private var running = false
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button {
            running = true
            Task {
                await action()
                running = false
            }
        } label: {
            Text(title)
                .font(.body.weight(.semibold))
                .foregroundStyle(.white)
                .lineLimit(1)
                .frame(maxWidth: .infinity)
                .frame(height: 48)
                .background(Theme.action, in: .capsule)
                .opacity(enabled && !running ? 1 : 0.45)
                .contentShape(.capsule)
        }
        .buttonStyle(.plain)
        .disabled(running)
    }
}

/// Who signs in: the person sharing the link, whose passkeys alone may sign.
private struct PasskeyCard: View {
    let person: FleetPerson
    let biometry: String

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "key.fill")
                .font(.body)
                .foregroundStyle(Theme.action)
                .frame(width: 40, height: 40)
                .background(Theme.action.opacity(0.16), in: .rect(cornerRadius: 10))
            VStack(alignment: .leading, spacing: 2) {
                Text(person.name)
                    .font(Theme.body.weight(.semibold)).foregroundStyle(Theme.text)
                Text("Your passkey, the one your other device uses. \(biometry) offers only that one.")
                    .font(.footnote).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(Theme.cellPadding)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .accessibilityElement(children: .combine)
    }
}

/// A machine of a shared fleet: its name, how it went, and for one that needs you, the fix and
/// Try Again. Its address is secondary, beside it.
private struct JoinRow: View {
    let row: FleetJoin.Row
    let device: String
    let biometry: String
    let retry: () async -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            JoinGlyph(state: row.state).frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: 3) {
                Text(row.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                Text(row.status(device: device, biometry: biometry)).font(.footnote).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if !row.fix(biometry: biometry).isEmpty {
                    Text(LocalizedStringKey(row.fix(biometry: biometry))).font(.footnote).foregroundStyle(Theme.tertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if row.state.needsYou {
                    ActionButton(title: "Try Again", style: .secondary, action: retry)
                        .frame(maxWidth: 120)
                        .padding(.top, 5)
                        .accessibilityIdentifier("retry-\(row.id)")
                }
            }
            Spacer(minLength: 8)
            if row.name != address {
                Text(address).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("join-\(row.id)")
    }

    private var address: String { FleetJoin.address(row.machine) }
}

/// A join row's mark, the same on every device: ✓ green when on it, `!` amber when it needs
/// you with a known fix, ✗ red for an error the machine did not explain, a spinner while it is
/// reached or signed in to, a green dot once reached.
private struct JoinGlyph: View {
    let state: FleetJoin.State

    var body: some View {
        switch state {
        case .added, .signedIn:
            Image(systemName: "checkmark").font(.body.weight(.heavy)).foregroundStyle(Theme.success)
        case .checking, .signingIn:
            ProgressView().controlSize(.small)
        case .ready:
            Circle().fill(Theme.success).frame(width: 9, height: 9)
        case .unreachable, .otherPasskey, .unknownPerson, .notVerified:
            Text("!").font(.body.weight(.black)).foregroundStyle(Theme.accent)
        case .failed:
            Image(systemName: "xmark").font(.body.weight(.heavy)).foregroundStyle(Theme.failure)
        }
    }
}
