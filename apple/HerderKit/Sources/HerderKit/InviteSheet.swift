import Herder
import SwiftUI

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
                      subtitle: "Scan the code with herder on their device, or send them the link to paste into Add to Fleet.",
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

/// The link an invite made, as the share sheet shows its own.
struct InviteLinkView: View {
    let invite: People.Invite
    var now: Date?

    var body: some View {
        PairingLinkCode(link: invite.link, expiresAt: invite.expiresAt,
                        hint: "It works once. Whoever uses it pairs as \(invite.name), \(invite.role == .owner ? "an owner" : "a member").",
                        now: now)
    }
}
