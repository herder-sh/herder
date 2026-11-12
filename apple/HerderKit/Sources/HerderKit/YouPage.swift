import Foundation
import Herder
import Observation
import SwiftUI

/// You, across the machines you are on: your passkeys, each with where it is kept, when it was
/// added and how many machines know it; your devices, each with how many machines it is on; and
/// the machines themselves. The You page and the Recovery page draw it.
struct You: Equatable {
    /// One of your passkeys, as the machines that know it list it.
    struct Passkey: Identifiable, Equatable {
        var key: PasskeyKey
        /// The machines that know you by it.
        var machines: [Directory.Column] = []
        /// Those of them that know you by it alone: removing it there would lock you out, so
        /// they keep it.
        var only: [Directory.Column] = []
        /// Whether this device signs with it.
        var thisDevice = false

        var id: String { key.credentialId }
        /// "1Password", from the AAGUID of the authenticator that made it; nil when it gave none.
        var store: String? { PasskeyStore.name(key.aaguid) }
        var addedAt: Date? { key.addedAt.flatMap(Timestamp.date) }
    }

    var person: Directory.Person
    /// The machines you are on.
    var columns: [Directory.Column]
    var passkeys: [Passkey]
    var devices: [Directory.Device]

    /// You from `directory`, with your passkeys as each machine lists them in `people`.
    /// `thisDevice` is the credential this device signs with.
    init(_ directory: Directory, people: [HostId: [PersonInfo]], thisDevice: String?) {
        let me = directory.me
        let columns = directory.columns.filter { me.roles[$0.id] != nil }
        var found: [String: Passkey] = [:]
        var order: [String] = []
        for column in columns {
            guard let mine = people[column.id]?.first(where: { $0.personId == me.id }) else { continue }
            for key in mine.passkeys {
                var passkey = found[key.credentialId] ?? Passkey(key: key, thisDevice: key.credentialId == thisDevice)
                if found[key.credentialId] == nil { order.append(key.credentialId) }
                // A machine added to before daemons kept these may lack them; another has them.
                if passkey.key.aaguid == nil { passkey.key.aaguid = key.aaguid }
                if let added = key.addedAt, passkey.key.addedAt.map({ added < $0 }) ?? true { passkey.key.addedAt = added }
                passkey.machines.append(column)
                if mine.passkeys.count == 1 { passkey.only.append(column) }
                found[key.credentialId] = passkey
            }
        }
        let passkeys = order.compactMap { found[$0] }
        self.init(person: me, columns: columns,
                  passkeys: passkeys.sorted { ($0.addedAt ?? .distantPast) < ($1.addedAt ?? .distantPast) },
                  devices: directory.devices)
    }

    init(person: Directory.Person, columns: [Directory.Column], passkeys: [Passkey], devices: [Directory.Device]) {
        self.person = person
        self.columns = columns
        self.passkeys = passkeys
        self.devices = devices
    }

    /// "Owner of 5 machines", "Owner of 3 machines · member of 2", "Member of 2 machines".
    var roles: String {
        let owned = columns.filter { person.roles[$0.id] == .owner }.count
        let member = columns.count - owned
        switch (owned, member) {
        case (0, 0): return "On no machines yet"
        case (_, 0): return "Owner of \(Self.machines(owned))"
        case (0, _): return "Member of \(Self.machines(member))"
        default: return "Owner of \(Self.machines(owned)) · member of \(member)"
        }
    }

    /// Whether losing one passkey still leaves you a way in.
    var covered: Bool { passkeys.count >= 2 }

    /// "2 passkeys in 2 places, and 3 devices."
    var coverage: String {
        let stores = passkeys.compactMap(\.store)
        let places = stores.count == passkeys.count ? Set(stores).count : nil
        let keys = passkeys.count == 1 ? "1 passkey" : "\(passkeys.count) passkeys"
        let devices = devices.count == 1 ? "1 device" : "\(devices.count) devices"
        if let places, places > 1 { return "\(keys) in \(places) places, and \(devices)." }
        return "\(keys) and \(devices)."
    }

    /// "on 5 machines", or "on 3 of 5 machines" when some do not know it.
    func reach(_ passkey: Passkey) -> String {
        passkey.machines.count == columns.count
            ? "on \(Self.machines(passkey.machines.count))"
            : "on \(passkey.machines.count) of \(Self.machines(columns.count))"
    }

    /// "On 5 of 5 machines", for a device.
    func reach(_ device: Directory.Device) -> String {
        "On \(device.paired.count) of \(Self.machines(columns.count))"
    }

    /// The passkeys that stay once `passkey` is gone, as a sentence: "You’ll still sign in with
    /// your 1Password passkey."
    func afterRemoving(_ passkey: Passkey) -> String {
        let others = passkeys.filter { $0.id != passkey.id }
        guard !others.isEmpty else {
            return "This is your only passkey, so it can’t be removed. Add another first."
        }
        let stores = others.compactMap(\.store)
        let still = stores.count == others.count && !stores.isEmpty
            ? "You’ll still sign in with your \(Directory.list(Array(Set(stores)).sorted())) passkey\(others.count > 1 ? "s" : "")."
            : "You’ll still sign in with your other passkey\(others.count > 1 ? "s" : "")."
        let forget = passkey.only.isEmpty
            ? " Every machine forgets this one."
            : " \(Directory.list(passkey.only.map(\.name))) \(passkey.only.count == 1 ? "knows" : "know") you by this one "
                + "alone, so \(passkey.only.count == 1 ? "it keeps" : "they keep") it until you add another there."
        return still + forget
    }

    nonisolated static func machines(_ count: Int) -> String {
        count == 1 ? "1 machine" : "\(count) machines"
    }

    /// "7Q2F…K9F9": a credential id, short.
    nonisolated static func short(_ credentialId: String) -> String {
        credentialId.count <= 10 ? credentialId : "\(credentialId.prefix(4))…\(credentialId.suffix(4))"
    }
}

/// Where a passkey is kept, from the AAGUID of the authenticator that made it, for the ones
/// people use most (the community list at github.com/passkeydeveloper/passkey-authenticator-aaguids).
enum PasskeyStore {
    nonisolated static func name(_ aaguid: String?) -> String? {
        aaguid.flatMap { known[$0.lowercased()] }
    }

    static let known: [String: String] = [
        "fbfc3007-154e-4ecc-8c0b-6e020557d7bd": "Apple Passwords",
        "dd4ec289-e01d-41c9-bb89-70fa845d4bf2": "Apple Passwords",
        "bada5566-a7aa-401f-bd96-45619a55120d": "1Password",
        "d548826e-79b4-db40-a3d8-11116f7e8349": "Bitwarden",
        "ea9b8d66-4d01-1d21-3ce4-b6b48cb575d4": "Google Password Manager",
        "adce0002-35bc-c60a-648b-0b25f1f05503": "Chrome on Mac",
        "531126d6-e717-415c-9320-3d9aa6981239": "Dashlane",
        "b84e4048-15dc-4dd0-8640-f4f60813c8af": "NordPass",
        "0ea242b4-43c4-4a1b-8b17-dd6d0b6baec6": "Keeper",
        "50726f74-6f6e-5061-7373-50726f746f6e": "Proton Pass",
        "fdb141b2-5d84-443e-8a35-4698c205a502": "KeePassXC",
        "08987058-cadc-4b81-b6e1-30de50dcbe96": "Windows Hello",
        "ee882879-721c-4913-9775-3dfcce97072a": "YubiKey",
        "2fc0579f-8113-47ea-b116-bb5a8db9202a": "YubiKey",
        "cb69481e-8ff7-4039-93ec-0a2729a154a8": "YubiKey",
        "a4e9fc6d-4cbe-4758-b8ba-37598bb5bbaa": "YubiKey",
        "149a2021-8ef6-4133-96b8-81f8d5b7f1f5": "YubiKey",
    ]
}

/// Adding another passkey of yours to every machine you are on, one machine after the other.
/// The first makes it: your passkey approves, then the new one is created from that machine's
/// registration. Every other machine learns it from the first's listing, with an assertion by
/// the new passkey and one by yours over its challenge. A machine that fails is left to try
/// again; closing Face ID stops.
@MainActor @Observable
final class PasskeyAdding {
    enum State: Equatable {
        case waiting
        case adding
        case added
        case failed(String)

        /// Whether adding it there is still to do, or to try again.
        var pending: Bool {
            switch self {
            case .waiting, .failed: true
            case .adding, .added: false
            }
        }

        var title: String {
            switch self {
            case .waiting: "Next"
            case .adding: "Adding…"
            case .added: "Added"
            case .failed(let why): why
            }
        }
    }

    struct Row: Identifiable, Equatable {
        let id: HostId
        let name: String
        var state = State.waiting
    }

    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult
    /// Creates the new passkey from a machine's registration, where it is to be kept.
    typealias Create = @MainActor (PasskeyCreation, PasskeyPlace) async throws -> PasskeyRegistration
    /// Signs a challenge with the new passkey alone, by its credential id.
    typealias SignNew = @MainActor (PasskeyChallenge, String, PasskeyPlace) async throws -> PasskeyAssertion

    let personId: PersonId
    var place = PasskeyPlace.passwordManager
    private(set) var rows: [Row]
    private(set) var running = false
    private(set) var started = false
    /// The new passkey, once a machine has it, as that machine lists it.
    private(set) var key: PasskeyKey?
    private let send: Send
    private let sign: PasskeySigner
    private let create: Create
    private let signNew: SignNew

    init(personId: PersonId, machines: [Directory.Column], send: @escaping Send, sign: @escaping PasskeySigner,
         create: @escaping Create, signNew: @escaping SignNew) {
        self.personId = personId
        rows = machines.map { Row(id: $0.id, name: $0.name) }
        self.send = send
        self.sign = sign
        self.create = create
        self.signNew = signNew
    }

    /// A failure after the new passkey was made but before a machine took it: trying the next
    /// machine would make yet another.
    private struct Stop: Error {
        let error: Error
    }

    func run() async {
        guard !running else { return }
        running = true
        started = true
        defer { running = false }
        for index in rows.indices where rows[index].state.pending {
            rows[index].state = .adding
            do {
                if let key {
                    try await spread(key, to: rows[index].id)
                } else {
                    key = try await make(on: rows[index].id)
                }
                rows[index].state = .added
            } catch let stop as Stop {
                rows[index].state = .failed(describe(stop.error))
                return
            } catch where isCancelled(error) {
                rows[index].state = .waiting
                return
            } catch {
                rows[index].state = .failed(describe(error))
            }
        }
    }

    /// Makes the passkey on `hostId`: what it lists for it.
    private func make(on hostId: HostId) async throws -> PasskeyKey {
        guard case .passkeyChallenge(let challenge, let creation?) = try await send(hostId, .passkeyChallenge) else {
            throw HerderError.Local(detail: "the machine sent nothing to make a passkey from")
        }
        let assertion = try await sign(challenge)
        let registration = try await create(creation, place)
        do {
            _ = try await send(hostId, .addPasskey(passkey: .register(registration: registration), assertion: assertion))
            guard case .people(let people) = try await send(hostId, .listPeople),
                  let key = people.first(where: { $0.personId == personId })?.passkeys
                      .first(where: { $0.credentialId == registration.id })
            else {
                throw HerderError.Local(detail: "the machine did not list the new passkey")
            }
            return key
        } catch {
            throw Stop(error: error)
        }
    }

    /// Introduces the passkey to `hostId`; one it has already counts as added.
    private func spread(_ key: PasskeyKey, to hostId: HostId) async throws {
        guard case .passkeyChallenge(let challenge, _) = try await send(hostId, .passkeyChallenge) else {
            throw HerderError.Local(detail: "the machine did not send a challenge")
        }
        let byKey = try await signNew(challenge, key.credentialId, place)
        let assertion = try await sign(challenge)
        do {
            _ = try await send(hostId, .addPasskey(passkey: .existing(personId: personId, key: key, assertion: byKey),
                                                  assertion: assertion))
        } catch HerderError.Rejected(let info) where info.code == .conflict {
            return
        }
    }

    var added: Int { rows.filter { $0.state == .added }.count }
    var pending: Int { rows.filter(\.state.pending).count }

    /// The sheet's one line: what Face ID asks, or how it went.
    func summary(biometry: String) -> String {
        guard started else {
            return "herder adds it to your \(You.machines(rows.count)). \(biometry) asks for each one."
        }
        if running { return "Added to \(added) of \(rows.count)… You can close this; it carries on." }
        if pending == 0 { return "Added to all \(rows.count)." }
        return "Added to \(added) of \(rows.count). Try again once the rest can be reached."
    }
}

extension You {
    /// Removes `passkey` from every machine that knows it, but those that know you by it alone,
    /// each with your passkey's assertion over its own challenge: one that stays must sign.
    @MainActor static func remove(_ passkey: Passkey, send: PasskeyAdding.Send, sign: PasskeySigner) async throws {
        var failures: [String] = []
        for column in passkey.machines where !passkey.only.contains(column) {
            do {
                guard case .passkeyChallenge(let challenge, _) = try await send(column.id, .passkeyChallenge) else {
                    throw HerderError.Local(detail: "the machine did not send a challenge")
                }
                let assertion = try await sign(challenge)
                _ = try await send(column.id, .removePasskey(credentialId: passkey.id, assertion: assertion))
            } catch where isCancelled(error) {
                throw error
            } catch {
                failures.append("\(column.name): \(describe(error))")
            }
        }
        if !failures.isEmpty { throw HerderError.Local(detail: failures.joined(separator: "\n")) }
    }
}

// MARK: - The You page

/// The You page: you, your passkeys, your devices, and how to get back in. Passkeys open their
/// sheet; Add a passkey adds one to every machine you are on.
struct YouPage: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var people: FleetPeople
    @State private var shown: YouSheet?
    @State private var signingOut: Directory.Device?
    @State private var error: String?
    @Environment(\.signPasskey) private var sign
    @Environment(\.pages) private var pages

    init(fleet: Fleet, sheet: Binding<AppSheet?>) {
        self.fleet = fleet
        _sheet = sheet
        _people = State(initialValue: FleetPeople(fleet: fleet))
    }

    var body: some View {
        let machines = fleet.machines
        let you = people.directory(machines).map { You($0, people: people.people, thisDevice: ThisDevicePasskey.credentialId) }
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                if let you {
                    YouHeader(you: you)
                    if let error {
                        Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("you-error")
                    }
                    YouSections(you: you,
                                open: { shown = .passkey($0.id) },
                                addPasskey: { shown = .add },
                                signOut: { signingOut = $0 },
                                addDevice: fleet.canShare ? { sheet = .addDevice } : nil,
                                recovery: { pages.open(.recovery) })
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
        .navigationTitle("You")
        .refreshable { await people.reload(machines) }
        .task(id: machines.filter { $0.connection == .connected }.map(\.hostId)) { await people.reload(machines) }
        .sheet(item: $shown, onDismiss: { Task { await people.reload(machines) } }) { shown in
            if let you {
                switch shown {
                case .add:
                    AddPasskeySheet(fleet: fleet, you: you, sign: sign)
                case .passkey(let id):
                    if let passkey = you.passkeys.first(where: { $0.id == id }) {
                        PasskeySheet(fleet: fleet, you: you, passkey: passkey, sign: sign)
                    }
                }
            }
        }
        .confirmationDialog("Sign out \(signingOut.map(YouSections.name) ?? "this device")?",
                            isPresented: Binding(get: { signingOut != nil }, set: { if !$0 { signingOut = nil } }),
                            titleVisibility: .visible, presenting: signingOut) { device in
            Button("Sign Out", role: .destructive) {
                error = nil
                Task {
                    do {
                        try await people.signOut(device, machines: machines, sign: sign)
                    } catch where isCancelled(error) {
                    } catch {
                        self.error = describe(error)
                    }
                }
            }
        } message: { _ in
            Text("Every machine forgets it. It can sign in again only with your passkey.")
        }
    }
}

/// What the You page shows over itself.
enum YouSheet: Identifiable, Hashable {
    case add
    /// A passkey, by its credential id.
    case passkey(String)

    var id: Self { self }
}

/// Your avatar, name and what you are on the machines.
struct YouHeader: View {
    let you: You

    var body: some View {
        HStack(spacing: 14) {
            Avatar(name: you.person.name, id: you.person.id, size: 52)
            VStack(alignment: .leading, spacing: 3) {
                Text(you.person.name).font(Theme.page).foregroundStyle(Theme.text).lineLimit(1)
                Text(you.roles).font(Theme.body).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.top, 14)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("you-header")
    }
}

/// The You page's sections: Passkeys, Devices and Recovery.
struct YouSections: View {
    let you: You
    let open: (You.Passkey) -> Void
    let addPasskey: () -> Void
    let signOut: (Directory.Device) -> Void
    /// Opens Add a Device, while this device has machines to share.
    var addDevice: (() -> Void)?
    let recovery: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            VStack(alignment: .leading, spacing: 8) {
                SectionHeading(title: "Passkeys", count: you.passkeys.count)
                SettingsGroup {
                    ForEach(you.passkeys) { passkey in
                        passkeyRow(passkey)
                        RowDivider()
                    }
                    AddRow(title: Self.adding("Add a passkey"), action: addPasskey)
                        .accessibilityIdentifier("you-add-passkey")
                }
                Text("A passkey is how machines know it’s you. Each one is a copy you can sign in with.")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            VStack(alignment: .leading, spacing: 8) {
                SectionHeading(title: "Devices", count: you.devices.count)
                SettingsGroup {
                    ForEach(Array(you.devices.enumerated()), id: \.element.id) { index, device in
                        if index > 0 { RowDivider() }
                        deviceRow(device)
                    }
                    if let addDevice {
                        if !you.devices.isEmpty { RowDivider() }
                        AddRow(title: Self.adding("Add a device"), action: addDevice)
                            .accessibilityIdentifier("you-add-device")
                    }
                }
            }
            VStack(alignment: .leading, spacing: 8) {
                SectionHeading(title: "Recovery")
                SettingsGroup {
                    Button(action: recovery) {
                        YouRow(symbol: "checkmark.shield", title: "If you lose your devices", summary: Text("How to get back in")) {
                            chevron
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("you-recovery")
                }
            }
        }
    }

    /// "Add a passkey…" on the Mac, where it opens a sheet; "Add a passkey" on the iPhone.
    static func adding(_ title: String) -> String {
        #if os(macOS)
        title + "…"
        #else
        title
        #endif
    }

    /// "This Mac", "This iPhone": this device, as its passkey row says it.
    static var thisDevice: String {
        #if os(macOS)
        "This Mac"
        #else
        "This iPhone"
        #endif
    }

    /// "Mac", "iPhone", "Terminal on trash-can-01".
    static func name(_ device: Directory.Device) -> String {
        switch device.kind {
        case .mac: "Mac"
        case .phone: "iPhone"
        case .terminal: device.host.map { "Terminal on \($0)" } ?? "Terminal"
        }
    }

    private var chevron: some View {
        Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
    }

    private func passkeyRow(_ passkey: You.Passkey) -> some View {
        let added = passkey.addedAt.map { "Added \($0.formatted(.dateTime.month(.abbreviated).day())) · " } ?? ""
        #if os(macOS)
        let summary = Text(added + you.reach(passkey))
        #else
        let summary = passkey.thisDevice
            ? Text("\(added + you.reach(passkey)) · \(Text("this iPhone uses it").fontWeight(.semibold))")
            : Text(added + you.reach(passkey))
        #endif
        return Button { open(passkey) } label: {
            YouRow(symbol: "key", title: you.person.name, tag: passkey.store, summary: summary) {
                #if os(macOS)
                if passkey.thisDevice { Pill(text: "\(Self.thisDevice) uses it", tint: Theme.action) }
                #endif
                chevron
            }
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("you-passkey-\(passkey.id)")
    }

    private func deviceRow(_ device: Directory.Device) -> some View {
        let summary = device.kind == .terminal
            ? Text("Signed in with \(Text("herder login").font(Theme.monoSmall)) · \(You.machines(device.paired.count))")
            : Text("\(you.reach(device)) · the \(device.kind == .mac ? "Mac" : "iPhone") app")
        return YouRow(symbol: device.symbol, title: Self.name(device), tag: device.current ? "This device" : nil,
                      summary: summary) {
            if !device.current {
                Button { signOut(device) } label: {
                    Text("Sign out…").font(.subheadline.weight(.medium)).foregroundStyle(Theme.secondary)
                        .padding(.horizontal, 8).frame(minHeight: 28).contentShape(.rect).hitTarget()
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Sign out \(Self.name(device))")
            }
        }
        .accessibilityIdentifier("you-device-\(device.id)")
    }
}

/// A row of the You page: a symbol in its tile, the title with an optional tag beside it, one
/// line of summary, then what goes at the end.
private struct YouRow<Trailing: View>: View {
    let symbol: String
    let title: String
    var tag: String?
    let summary: Text
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.body)
                .foregroundStyle(Theme.secondary)
                .frame(width: 32, height: 32)
                .background(Theme.raised, in: .rect(cornerRadius: 8))
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text(title).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                    if let tag { Pill(text: tag).fixedSize() }
                }
                summary.font(.caption).foregroundStyle(Theme.secondary).lineLimit(2)
            }
            .layoutPriority(1)
            Spacer(minLength: 8)
            trailing
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 10)
        .frame(minHeight: Theme.tallRowHeight)
        .contentShape(.rect)
    }
}

extension Directory.Device {
    var symbol: String {
        switch kind {
        case .mac: "laptopcomputer"
        case .phone: "iphone"
        case .terminal: "apple.terminal"
        }
    }
}

// MARK: - A passkey's sheet

/// One passkey: where it is kept, when it was added, the machines that know it and its
/// credential id, with Remove, which says what still works afterwards. The last passkey has no
/// Remove.
struct PasskeySheet: View {
    let fleet: Fleet
    let you: You
    let passkey: You.Passkey
    let sign: PasskeySigner
    @State private var confirming = false
    @State private var removing = false
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        PasskeyDetails(you: you, passkey: passkey, removing: removing, error: error) { confirming = true }
            .confirmationDialog("Remove this passkey?", isPresented: $confirming, titleVisibility: .visible) {
                Button("Remove Passkey", role: .destructive) { remove() }
            } message: {
                Text(you.afterRemoving(passkey))
            }
    }

    private func remove() {
        let client = fleet.client
        removing = true
        error = nil
        Task {
            defer { removing = false }
            do {
                try await You.remove(passkey, send: { try await client.send(hostId: $0, command: $1) }, sign: sign)
                dismiss()
            } catch where isCancelled(error) {
            } catch {
                self.error = describe(error)
            }
        }
    }
}

/// What `PasskeySheet` draws.
struct PasskeyDetails: View {
    let you: You
    let passkey: You.Passkey
    var removing = false
    var error: String?
    let remove: () -> Void

    var body: some View {
        DetailSheet(title: [you.person.name, passkey.store].compactMap(\.self).joined(separator: " · "),
                    destructive: you.passkeys.count > 1 ? (removing ? "Removing…" : "Remove Passkey…") : "",
                    onDestructive: { if !removing { remove() } }) {
            Text(passkey.store.map { "Saved in \($0)" } ?? "Saved where you made it: the machines were not told where.")
                .font(.subheadline).foregroundStyle(Theme.secondary)
            if let added = passkey.addedAt {
                SettingsGroup {
                    DetailRow(label: "Added", value: added.formatted(.dateTime.month(.abbreviated).day().year()))
                }
            }
            VStack(alignment: .leading, spacing: 8) {
                SectionHeading(title: "Known by \(You.machines(passkey.machines.count))")
                SettingsGroup {
                    ForEach(Array(passkey.machines.enumerated()), id: \.element.id) { index, machine in
                        if index > 0 { RowDivider() }
                        HStack(spacing: 10) {
                            Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.success)
                            Text(machine.name).font(Theme.body).foregroundStyle(Theme.text)
                            Spacer()
                        }
                        .padding(.horizontal, Theme.cellPadding)
                        .frame(minHeight: Theme.rowHeight)
                    }
                }
            }
            HStack(spacing: 6) {
                Text("Credential")
                Text(You.short(passkey.id)).font(Theme.monoSmall).textSelection(.enabled)
            }
            .font(.caption).foregroundStyle(Theme.tertiary)
            Text(you.afterRemoving(passkey))
                .font(.footnote).foregroundStyle(Theme.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityIdentifier("passkey-sheet")
    }
}

// MARK: - Add a passkey

/// Add a passkey: where to keep it, then adding it to every machine you are on, live.
struct AddPasskeySheet: View {
    @State private var adding: PasskeyAdding

    init(fleet: Fleet, you: You, sign: @escaping PasskeySigner, authenticator: Authenticator = passkeyAuthenticator()) {
        let client = fleet.client
        let connected = Set(fleet.machines.filter { $0.connection == .connected }.map(\.hostId))
        // The connected machines first, so the passkey is made on one that answers.
        let machines = you.columns.filter { connected.contains($0.id) } + you.columns.filter { !connected.contains($0.id) }
        let name = you.person.name
        _adding = State(initialValue: PasskeyAdding(
            personId: you.person.id, machines: machines,
            send: { try await client.send(hostId: $0, command: $1) },
            sign: sign,
            create: { creation, place in
                guard let challenge = Data(base64URL: creation.challenge), let user = Data(base64URL: creation.userHandle) else {
                    throw HerderError.Local(detail: "the machine sent a registration that is not base64url")
                }
                return try await authenticator.register(name: name, userId: user, challenge: challenge, on: place)
            },
            signNew: { challenge, credentialId, place in
                guard let bytes = Data(base64URL: challenge.challenge), let id = Data(base64URL: credentialId) else {
                    throw HerderError.Local(detail: "the machine sent a challenge that is not base64url")
                }
                return try await authenticator.assert(challenge: bytes, credentialId: id, on: place)
            }))
    }

    var body: some View {
        AddPasskeyView(adding: adding)
    }
}

/// What `AddPasskeySheet` draws: the two places to keep it, then each machine with how adding
/// it there went.
struct AddPasskeyView: View {
    @Bindable var adding: PasskeyAdding
    var biometry = Biometry.name
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        SheetScaffold(title: "Add a Passkey", subtitle: "Keep a second passkey, so losing one never locks you out.",
                      height: 560, hint: adding.summary(biometry: biometry)) {
            if adding.started {
                SettingsGroup {
                    ForEach(Array(adding.rows.enumerated()), id: \.element.id) { index, row in
                        if index > 0 { RowDivider() }
                        AddingRow(row: row)
                    }
                }
            } else {
                VStack(spacing: 10) {
                    PlaceChoice(symbol: "key", title: "In another password manager",
                                detail: "1Password, Bitwarden… you pick it next. Not the one this device uses.",
                                picked: adding.place == .passwordManager) { adding.place = .passwordManager }
                    PlaceChoice(symbol: "key.horizontal", title: "On a security key", detail: "A YubiKey or similar.",
                                picked: adding.place == .securityKey) { adding.place = .securityKey }
                }
            }
        } footer: {
            Spacer()
            if !adding.started {
                SettingsButton(title: "Create Passkey", style: .primary) { Task { await adding.run() } }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("create-passkey")
            } else if adding.pending > 0 {
                SettingsButton(title: "Done") { dismiss() }
                SettingsButton(title: adding.running ? "Adding…" : "Try Again", style: .primary) {
                    Task { await adding.run() }
                }
                .disabled(adding.running)
                .keyboardShortcut(.defaultAction)
            } else {
                SettingsButton(title: "Done", style: .primary) { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
        }
    }
}

/// One place to keep the new passkey, ticked when picked.
private struct PlaceChoice: View {
    let symbol: String
    let title: String
    let detail: String
    let picked: Bool
    let pick: () -> Void

    var body: some View {
        Button(action: pick) {
            HStack(spacing: 12) {
                Image(systemName: symbol).font(.title3).foregroundStyle(Theme.secondary).frame(width: 32)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(Theme.body.weight(.semibold)).foregroundStyle(Theme.text)
                    Text(detail).font(.caption).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                Image(systemName: picked ? "checkmark.circle.fill" : "circle")
                    .font(.title3).foregroundStyle(picked ? Theme.action : Theme.tertiary)
            }
            .padding(Theme.cellPadding)
            .background(Theme.cardInSheet, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(picked ? Theme.action : Theme.stroke))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(picked ? .isSelected : [])
    }
}

/// A machine the new passkey goes to: ✓ added, a spinner while adding, a dashed ring waiting,
/// and `!` with why where it failed.
private struct AddingRow: View {
    let row: PasskeyAdding.Row

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            glyph.frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                Text(row.state.title).font(.caption).foregroundStyle(tint)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("adding-\(row.id)")
    }

    @ViewBuilder private var glyph: some View {
        switch row.state {
        case .adding: ProgressView().controlSize(.small).tint(Theme.running)
        case .added: Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.success)
        case .waiting: Image(systemName: "circle.dashed").foregroundStyle(Theme.tertiary)
        case .failed: Image(systemName: "exclamationmark.circle.fill").foregroundStyle(Theme.accent)
        }
    }

    private var tint: Color {
        switch row.state {
        case .added: Theme.success
        case .failed: Theme.secondary
        case .waiting, .adding: Theme.tertiary
        }
    }
}

// MARK: - Recovery

/// If you lose your devices: how to get back in, written down before it is needed, and whether
/// you are covered.
struct RecoveryPage: View {
    let fleet: Fleet
    @State private var people: FleetPeople
    @State private var adding = false
    @Environment(\.signPasskey) private var sign

    init(fleet: Fleet) {
        self.fleet = fleet
        _people = State(initialValue: FleetPeople(fleet: fleet))
    }

    var body: some View {
        let machines = fleet.machines
        let you = people.directory(machines).map { You($0, people: people.people, thisDevice: ThisDevicePasskey.credentialId) }
        ScrollView {
            RecoveryView(you: you) { adding = true }
                .pageColumn()
        }
        .background(.ground)
        .navigationTitle("If you lose your devices")
        .task(id: machines.filter { $0.connection == .connected }.map(\.hostId)) { await people.reload(machines) }
        .sheet(isPresented: $adding, onDismiss: { Task { await people.reload(machines) } }) {
            if let you { AddPasskeySheet(fleet: fleet, you: you, sign: sign) }
        }
    }
}

/// What `RecoveryPage` draws: whether you are covered, then the three cases.
struct RecoveryView: View {
    /// Nil until the machines have listed you.
    let you: You?
    let addPasskey: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("If you lose your devices").font(Theme.page).foregroundStyle(Theme.text).padding(.top, 14)
            if let you {
                if you.covered {
                    status(symbol: "checkmark", tint: Theme.switchOn, title: "You’re covered", detail: you.coverage)
                        .accessibilityIdentifier("recovery-covered")
                } else {
                    HStack(alignment: .center, spacing: 12) {
                        status(symbol: "key", tint: Theme.action, title: "Add a second passkey",
                               detail: "With one passkey, losing it locks you out of every machine.")
                        SettingsButton(title: YouSections.adding("Add a passkey"), style: .primary, action: addPasskey)
                            .fixedSize()
                    }
                    .accessibilityIdentifier("recovery-add-passkey")
                }
            }
            SettingsGroup {
                RecoveryCase(symbol: "iphone", title: "Lost one device",
                             detail: "Sign it out from any other: You › Devices. It can’t use your machines after that.")
                RowDivider()
                RecoveryCase(symbol: "key", title: "Lost every device",
                             detail: "Your passkey is synced. On a new device choose **I already use herder › I have no other device**, then run `herder claim` on any machine and scan it.")
                RowDivider()
                RecoveryCase(symbol: "apple.terminal", title: "Lost your passkeys too",
                             detail: "Anyone with a shell on a machine can run `herder claim` to become its owner. Do it on each, then remove the old you.")
            }
        }
    }

    private func status(symbol: String, tint: Color, title: String, detail: String) -> some View {
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.body.weight(.bold))
                .foregroundStyle(tint)
                .frame(width: 32, height: 32)
                .background(tint.opacity(0.13), in: .rect(cornerRadius: 8))
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(Theme.body.weight(.semibold)).foregroundStyle(Theme.text)
                Text(detail).font(.caption).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
    }
}

/// One way to get back in: what was lost, and what to do.
private struct RecoveryCase: View {
    let symbol: String
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: symbol)
                .font(.body)
                .foregroundStyle(Theme.secondary)
                .frame(width: 32, height: 32)
                .background(Theme.raised, in: .rect(cornerRadius: 8))
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Text(LocalizedStringKey(detail)).font(.caption).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
    }
}

// MARK: - Demo

/// The You the mockup draws (design/onboarding, flow 4): Tomas, owner of five machines, with a
/// passkey in Apple Passwords this device uses and one in 1Password, a Mac, an iPhone and a
/// terminal. The screenshot tests draw it.
extension You {
    static func demo(now: Date) -> You {
        let names = ["trash-can-01", "draugelis", "work-mbp", "mini-k8s", "studio"]
        let columns = names.map { Directory.Column(id: $0, name: $0) }
        let all = Dictionary(uniqueKeysWithValues: names.map { ($0, [$0 + "-device"]) })
        let apple = PasskeyKey(credentialId: "Ap9xK2mQ7vLwR4tZ", publicKey: "{}",
                               aaguid: "fbfc3007-154e-4ecc-8c0b-6e020557d7bd", addedAt: "2026-10-03T09:00:00Z")
        let onePassword = PasskeyKey(credentialId: "7Q2FbN8sHc1JdK9F9", publicKey: "{}",
                                     aaguid: "bada5566-a7aa-401f-bd96-45619a55120d", addedAt: "2026-11-11T09:00:00Z")
        return You(
            person: Directory.Person(id: "tomas-1", name: "Tomas",
                                     roles: Dictionary(uniqueKeysWithValues: names.map { ($0, .owner) })),
            columns: columns,
            passkeys: [Passkey(key: apple, machines: columns, thisDevice: true), Passkey(key: onePassword, machines: columns)],
            devices: [
                Directory.Device(id: "herder-ios", kind: .phone, lastSeen: now.addingTimeInterval(-2 * 3600), paired: all),
                Directory.Device(id: "herder-macos", kind: .mac, lastSeen: now, current: true, paired: all),
                Directory.Device(id: "herder-tui", kind: .terminal, host: "trash-can-01",
                                 lastSeen: now.addingTimeInterval(-86400), paired: ["trash-can-01": ["tui"]]),
            ])
    }
}
