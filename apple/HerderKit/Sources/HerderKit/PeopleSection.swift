import Herder
import SwiftUI

/// A machine's People section: each user with their role and, under them, their devices.
/// Owners change a role, revoke a device and invite someone new; members see the list only.
struct PeopleSection: View {
    let fleet: Fleet
    let machine: Machine
    @State private var people: People
    @State private var inviting = false
    @State private var revoking: UserDevice?
    @State private var error: String?

    init(fleet: Fleet, machine: Machine) {
        self.fleet = fleet
        self.machine = machine
        _people = State(initialValue: People(fleet: fleet, hostId: machine.hostId))
    }

    var body: some View {
        PeopleList(people: people.users, owner: machine.role == .owner, error: error,
                   setRole: { role, user in run { try await people.setRole(role, of: user) } },
                   revoke: { revoking = $0 },
                   invite: { inviting = true })
            .task(id: machine.connection == .connected) {
                if machine.connection == .connected { await people.reload() }
            }
            .sheet(isPresented: $inviting) { InviteSheet(people: people, machine: machine.name) }
            .confirmationDialog("Revoke this device?", isPresented: Binding(
                get: { revoking != nil }, set: { if !$0 { revoking = nil } }), titleVisibility: .visible) {
                Button("Revoke", role: .destructive) {
                    guard let device = revoking else { return }
                    run { try await people.revoke(device) }
                }
            } message: {
                Text("It is disconnected now, and pairs again only with a new code.")
            }
    }

    private func run(_ action: @escaping () async throws -> Void) {
        error = nil
        Task {
            do { try await action() } catch { self.error = describe(error) }
        }
    }
}

/// The People list itself, from what the machine answered: a view of its own so tests draw
/// each state.
struct PeopleList: View {
    let people: Load<[UserInfo]>
    /// Whether this device's user owns the machine, and so may change it.
    let owner: Bool
    var error: String?
    var now = Date.now
    let setRole: (Role, UserInfo) -> Void
    let revoke: (UserDevice) -> Void
    let invite: () -> Void

    var body: some View {
        SettingsList(title: "People") {
            switch people {
            case .asking:
                SettingRow(label: "People") { ProgressView().controlSize(.small) }
            case .failed(let why):
                SettingRow(label: "Cannot list people", hint: why) { EmptyView() }
            case .loaded(let users):
                ForEach(Array(users.enumerated()), id: \.element.userId) { index, user in
                    if index > 0 { RowDivider() }
                    userRow(user, lastOwner: People.isLastOwner(user, in: users))
                    ForEach(user.devices, id: \.deviceId) { device in
                        RowDivider().padding(.leading, 24)
                        deviceRow(device, of: user)
                    }
                }
                if owner {
                    RowDivider()
                    SettingRow(label: "Invite someone", hint: "A one-time code pairs their device as a member or an owner") {
                        PillButton(title: "Invite…", symbol: "person.badge.plus", prominent: true, action: invite)
                            .accessibilityIdentifier("people-invite")
                    }
                }
            }
            if let error {
                RowDivider()
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    .padding(.horizontal, Theme.cellPadding).frame(minHeight: 36)
            }
        }
    }

    private func userRow(_ user: UserInfo, lastOwner: Bool) -> some View {
        SettingRow(label: user.name, hint: devices(user)) {
            RoleChip(role: user.role)
            if owner {
                Menu {
                    if user.role == .member {
                        Button("Make owner", systemImage: "person.badge.key") { setRole(.owner, user) }
                    } else {
                        Button("Make member", systemImage: "person") { setRole(.member, user) }
                            .disabled(lastOwner)
                    }
                } label: {
                    Image(systemName: "ellipsis").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.secondary)
                        .frame(width: 28, height: 28).contentShape(.rect)
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
                .accessibilityLabel("Role of \(user.name)")
            }
        }
        .accessibilityIdentifier("people-user-\(user.name)")
    }

    private func deviceRow(_ device: UserDevice, of user: UserInfo) -> some View {
        SettingRow(label: People.deviceName(device.client), hint: People.detail(device, now: now)) {
            if owner {
                PillButton(title: "Revoke…") { revoke(device) }
                    .accessibilityLabel("Revoke \(user.name)'s \(People.deviceName(device.client))")
            }
        }
        .padding(.leading, 24)
    }

    /// "2 devices", or that every one was revoked.
    private func devices(_ user: UserInfo) -> String {
        switch user.devices.count {
        case 0: "No devices: invite them again to pair one"
        case 1: "1 device"
        case let count: "\(count) devices"
        }
    }
}

/// A user's role, as a chip: owners blue, members neutral.
struct RoleChip: View {
    let role: Role

    var body: some View {
        Chip(text: role == .owner ? "Owner" : "Member", tint: role == .owner ? Theme.link : Theme.secondary)
    }
}

/// Invites someone to a machine: their name and role, then the one-time link that pairs their
/// device, as a QR code to scan and text to copy.
struct InviteSheet: View {
    let people: People
    let machine: String
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var role = Role.member
    @State private var invite: People.Invite?
    @State private var error: String?

    var body: some View {
        SheetScaffold(title: "Invite to \(machine)",
                      subtitle: "Scan the code with herder on their device, or send them the link to paste into Add Machine.",
                      height: 720) {
            if let invite {
                InviteLinkView(invite: invite)
            } else {
                Field(label: "Name", hint: "Their name on \(machine). Someone already here keeps their role.") {
                    InputBox(placeholder: "alex", text: $name)
                        .accessibilityIdentifier("invite-name")
                }
                Field(label: "Role", hint: role == .owner
                      ? "Full control, terminals included."
                      : "Drives sessions; never gets a terminal.") {
                    Picker("Role", selection: $role) {
                        Text("Member").tag(Role.member)
                        Text("Owner").tag(Role.owner)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .fixedSize()
                    .accessibilityIdentifier("invite-role")
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                }
            }
        } footer: {
            Spacer()
            if invite != nil {
                ActionButton(title: "New Code", style: .secondary) { await mint() }
                    .frame(maxWidth: 160)
                ActionButton(title: "Done", style: .primary) { dismiss() }
                    .frame(maxWidth: 160)
                    .keyboardShortcut(.defaultAction)
            } else {
                ActionButton(title: "Make Code", style: .primary) { await mint() }
                    .frame(maxWidth: 160)
                    .keyboardShortcut(.defaultAction)
                    .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    private func mint() async {
        do {
            invite = try await people.invite(name, as: role)
            error = nil
        } catch {
            invite = nil
            self.error = describe(error)
        }
    }
}

/// The link an invite made, as `ShareSheet` shows its own: a QR code with its expiry, and the
/// link to copy.
struct InviteLinkView: View {
    let invite: People.Invite
    var now: Date?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let at = now ?? context.date
            let expired = invite.expiresAt.map { $0 <= at } ?? false
            VStack(spacing: 12) {
                if let code = QRCode.image(invite.link) {
                    Image(decorative: code, scale: 1)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 220, height: 220)
                        .padding(14)
                        .background(.white, in: .rect(cornerRadius: Theme.corner))
                        .opacity(expired ? 0.15 : 1)
                        .accessibilityLabel("QR code of the invite link")
                }
                Text(expiry(invite.expiresAt, now: at))
                    .font(.subheadline.weight(.medium).monospacedDigit())
                    .foregroundStyle(expired ? Theme.failure : Theme.secondary)
            }
            .frame(maxWidth: .infinity)
        }
        Field(label: "Link", hint: "It works once. Whoever uses it pairs as \(invite.name), \(invite.role == .owner ? "an owner" : "a member").") {
            HStack(alignment: .top) {
                Text(invite.link)
                    .font(Theme.monoSmall)
                    .foregroundStyle(Theme.text)
                    .lineLimit(3)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Spacer()
                CopyButton(text: invite.link)
            }
            .padding(12)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        }
    }
}
