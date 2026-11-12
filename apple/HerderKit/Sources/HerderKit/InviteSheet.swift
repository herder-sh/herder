import Herder
import Observation
import SwiftUI

/// Invite a teammate: an optional name, the role, and the machines, then one link that lets
/// them join each machine once with a passkey of their own, as a QR code, a share and a copy.
/// Each machine mints its own invite, so the link covers the machines ticked now; a machine
/// added later needs another invite. Minting asks for your passkey once per machine.
struct InviteSheet: View {
    let fleet: Fleet
    @State private var maker: InviteMaker
    @Environment(\.signPasskey) private var sign

    /// `only` ticks that machine alone; every one you can invite to otherwise.
    init(fleet: Fleet, only: HostId? = nil) {
        self.fleet = fleet
        let people = FleetPeople(fleet: fleet)
        let machines = InviteMaker.invitable(fleet.machines)
        _maker = State(initialValue: InviteMaker(machines: machines, only: only) { [people] role, machine, sign in
            try await people.invite(role, on: machine.hostId, name: machine.name, sign: sign)
        })
    }

    var body: some View {
        InviteSheetView(maker: maker, sign: sign)
            .task {
                let people = FleetPeople(fleet: fleet)
                await people.reload(fleet.machines)
                let me = FleetPeople.me(fleet.machines)
                maker.by = people.people.values.joined().first { $0.personId == me }?.name
            }
    }
}

/// Mints one invite per ticked machine and folds them into one link.
@MainActor @Observable
final class InviteMaker {
    struct Machine: Identifiable, Equatable {
        let hostId: HostId
        let name: String
        var id: HostId { hostId }
    }

    /// Mints `machine`'s invite for `role`, signed by your passkey.
    typealias Mint = @MainActor (Role, Machine, @escaping PasskeySigner) async throws -> FleetPeople.Invite

    let machines: [Machine]
    /// Who it is for; only labels the link.
    var name = ""
    var role = Role.member
    var picked: Set<HostId>
    /// You, as the link names who invited them.
    var by: String?
    private(set) var link: InviteLink?
    /// When the first of its invites stops working.
    private(set) var expiresAt: Date?
    /// The machines that did not mint an invite, each with why: "studio: …".
    private(set) var failures: [String] = []
    private let mint: Mint

    init(machines: [Machine], only: HostId? = nil, mint: @escaping Mint) {
        self.machines = machines
        self.mint = mint
        picked = Set(only.map { [$0] } ?? machines.map(\.hostId))
    }

    /// The machines you can invite to: those you own, connected, not vaults.
    nonisolated static func invitable(_ machines: [Herder.Machine]) -> [Machine] {
        machines.filter { $0.role == .owner && $0.connection == .connected && $0.hosts.isEmpty && $0.vault == nil }
            .map { Machine(hostId: $0.hostId, name: $0.name) }
    }

    var allPicked: Bool { picked.count == machines.count }

    /// Asks each ticked machine for an invite, in order, and makes the link of those that gave
    /// one. Closing Face ID stops, leaving the link of those minted so far.
    func make(sign: @escaping PasskeySigner) async {
        failures = []
        var minted: [FleetPeople.Invite] = []
        for machine in machines where picked.contains(machine.hostId) {
            do {
                minted.append(try await mint(role, machine, sign))
            } catch where isCancelled(error) {
                break
            } catch {
                failures.append("\(machine.name): \(describe(error))")
            }
        }
        guard !minted.isEmpty else { return }
        let label = name.trimmingCharacters(in: .whitespaces)
        link = InviteLink(machines: minted.map(\.machine), role: role, by: by, label: label.isEmpty ? nil : label)
        expiresAt = minted.compactMap(\.expiresAt).min()
    }

    /// Back to the form, for another link.
    func reset() {
        link = nil
        expiresAt = nil
        failures = []
    }
}

/// The sheet: the form until the link is made, then the link.
struct InviteSheetView: View {
    let maker: InviteMaker
    let sign: PasskeySigner
    var now: Date?
    var biometry = Biometry.name

    var body: some View {
        if let link = maker.link {
            SheetScaffold(title: link.label.map { "Invite for \($0)" } ?? "Invite a Teammate",
                          subtitle: link.summary,
                          height: 640) {
                InviteLinkPanel(link: link, expiresAt: maker.expiresAt, failures: maker.failures, now: now)
            } footer: {
                Spacer()
                InviteLinkButtons(link: link.string)
            }
        } else {
            SheetScaffold(title: "Invite a Teammate",
                          subtitle: "They open the link in herder and join with a passkey of their own.",
                          height: 640,
                          hint: maker.picked.count > 1 ? "\(biometry) asks once for each machine." : "") {
                InviteForm(maker: maker)
            } footer: {
                Spacer()
                ActionButton(title: "Create Invite Link", style: .primary) { await maker.make(sign: sign) }
                    .frame(maxWidth: 220)
                    .disabled(maker.picked.isEmpty)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("invite-create")
            }
        }
    }
}

/// Who it is for, as what, and on which machines.
struct InviteForm: View {
    @Bindable var maker: InviteMaker

    var body: some View {
        Field(label: "Name, optional") {
            InputBox(placeholder: "Lina", text: $maker.name)
                .accessibilityIdentifier("invite-name")
        }
        Field(label: "Role", hint: Self.meaning(maker.role)) {
            Picker("Role", selection: $maker.role) {
                Text("Member").tag(Role.member)
                Text("Owner").tag(Role.owner)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(maxWidth: 260)
            .accessibilityIdentifier("invite-role")
        }
        SettingsList(title: "Machines", caption: "A machine you add later needs its own invite.") {
            if maker.machines.isEmpty {
                EmptyRow(text: "You own no connected machine to invite to.")
            } else {
                SwitchRow(title: "All my machines", detail: "\(maker.machines.count) now", isOn: Binding(
                    get: { maker.allPicked },
                    set: { maker.picked = $0 ? Set(maker.machines.map(\.hostId)) : [] }))
                    .accessibilityIdentifier("invite-all")
                ForEach(maker.machines) { machine in
                    RowDivider()
                    MachineCheck(name: machine.name, isOn: Binding(
                        get: { maker.picked.contains(machine.hostId) },
                        set: { on in
                            if on { maker.picked.insert(machine.hostId) } else { maker.picked.remove(machine.hostId) }
                        }))
                }
            }
        }
        if !maker.failures.isEmpty {
            Text(maker.failures.joined(separator: "\n")).font(.footnote).foregroundStyle(Theme.failure)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// What the role lets them do, in one line.
    nonisolated static func meaning(_ role: Role) -> String {
        role == .owner ? "Can do everything, terminals and secrets included." : "Drives sessions. Never gets a terminal."
    }
}

/// A machine to tick in the invite's checklist.
private struct MachineCheck: View {
    let name: String
    @Binding var isOn: Bool

    var body: some View {
        Button { isOn.toggle() } label: {
            HStack(spacing: 10) {
                Image(systemName: isOn ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(isOn ? Theme.action : Theme.tertiary)
                Text(name).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                Spacer()
            }
            .padding(.horizontal, Theme.cellPadding)
            .frame(minHeight: Theme.rowHeight)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isOn ? .isSelected : [])
        .accessibilityIdentifier("invite-machine-\(name)")
    }
}

/// The link: its QR code, what it grants, that it works once and until when, and the link
/// itself in short; what did not mint under it.
struct InviteLinkPanel: View {
    let link: InviteLink
    let expiresAt: Date?
    var failures: [String] = []
    /// The clock the expiry counts down on; the live one when absent.
    var now: Date?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let at = now ?? context.date
            let expired = expiresAt.map { $0 <= at } ?? false
            VStack(spacing: 6) {
                if let code = QRCode.image(link.string) {
                    Image(decorative: code, scale: 1)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 210, height: 210)
                        .padding(14)
                        .background(.white, in: .rect(cornerRadius: Theme.corner))
                        .opacity(expired ? 0.15 : 1)
                        .accessibilityLabel("QR code of the invite link")
                        .padding(.bottom, 8)
                }
                Text(link.summary)
                    .font(.body.weight(.semibold)).foregroundStyle(Theme.text)
                Text(Self.terms(expiresAt, now: at))
                    .font(.subheadline.monospacedDigit())
                    .foregroundStyle(expired ? Theme.failure : Theme.tertiary)
                Text(link.string)
                    .font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                    .lineLimit(1).truncationMode(.middle)
                    .textSelection(.enabled)
                    .padding(.horizontal, 24)
            }
            .frame(maxWidth: .infinity)
        }
        if !failures.isEmpty {
            Label {
                Text("Not in this invite:\n" + failures.joined(separator: "\n"))
            } icon: {
                Image(systemName: "exclamationmark.circle")
            }
            .font(.footnote).foregroundStyle(Theme.accent)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// "Works once · expires in 9:41", or that it expired.
    nonisolated static func terms(_ expiresAt: Date?, now: Date) -> String {
        guard let expiresAt else { return "Works once" }
        guard expiresAt > now else { return expiry(expiresAt, now: now) }
        return "Works once · " + expiry(expiresAt, now: now).replacing("Expires", with: "expires")
    }
}

/// Copy Link, and Share… through the system's share sheet.
struct InviteLinkButtons: View {
    let link: String
    @State private var copied = false

    var body: some View {
        ActionButton(title: copied ? "Copied" : "Copy Link", style: .secondary) {
            Clipboard.string = link
            copied = true
        }
        .frame(maxWidth: 150)
        .accessibilityIdentifier("invite-copy")
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
        if let url = URL(string: link) {
            ShareLink(item: url, subject: Text("Join me on herder")) {
                Label("Share…", systemImage: "square.and.arrow.up")
                    .font(.body.weight(.semibold))
                    .frame(maxWidth: .infinity)
                    .frame(height: 48)
                    .foregroundStyle(Theme.text)
                    .primaryFill(in: .rect(cornerRadius: Theme.corner))
                    .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .frame(maxWidth: 150)
            .keyboardShortcut(.defaultAction)
            .accessibilityIdentifier("invite-share")
        }
    }
}
