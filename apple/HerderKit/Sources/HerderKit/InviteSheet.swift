import Herder
import SwiftUI

/// Invites someone to a machine as a member or an owner: a one-time link that lets them join
/// with a passkey of their own, as a QR code to scan and text to copy. Minting it asks for your
/// passkey.
struct InviteSheet: View {
    let fleet: Fleet
    let hostId: HostId
    @Environment(\.dismiss) private var dismiss
    @Environment(\.signPasskey) private var sign
    @State private var role = Role.member
    @State private var invite: FleetPeople.Invite?
    @State private var error: String?

    private var machine: String { fleet.machines.first { $0.hostId == hostId }?.name ?? "the machine" }

    var body: some View {
        SheetScaffold(title: "Invite to \(machine)",
                      subtitle: "They scan the code with herder, or open the link, and join with a passkey of their own.",
                      height: 720) {
            if let invite {
                InviteLinkView(invite: invite)
            } else {
                Field(label: "Role", hint: role == .owner
                      ? "Full control, terminals and secrets included."
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
                ActionButton(title: "New Link", style: .secondary) { await mint() }
                    .frame(maxWidth: 160)
                ActionButton(title: "Done", style: .primary) { dismiss() }
                    .frame(maxWidth: 160)
                    .keyboardShortcut(.defaultAction)
            } else {
                ActionButton(title: "Make Link", style: .primary) { await mint() }
                    .frame(maxWidth: 160)
                    .keyboardShortcut(.defaultAction)
            }
        }
    }

    private func mint() async {
        do {
            invite = try await FleetPeople(fleet: fleet).invite(role, on: hostId, sign: sign)
            error = nil
        } catch {
            invite = nil
            self.error = describe(error)
        }
    }
}

/// The link an invite made, as the share sheet shows its own.
struct InviteLinkView: View {
    let invite: FleetPeople.Invite
    var now: Date?

    var body: some View {
        InviteLinkCode(link: invite.link, expiresAt: invite.expiresAt,
                        hint: "It works once. Whoever uses it joins as \(invite.role == .owner ? "an owner" : "a member"), with a passkey of their own.",
                        now: now)
    }
}
