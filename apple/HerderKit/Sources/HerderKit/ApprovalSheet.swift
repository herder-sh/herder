import Herder
import SwiftUI

/// Approving a new machine or a terminal sign-in: what asks, the large code to compare with the
/// terminal's, the fingerprint, then Approve with Face ID or Not mine; once approved, what comes
/// next.
struct ApprovalSheet: View {
    let approval: Approval
    /// Your teammates' names, who get no access to a new machine until you add them.
    var teammates: [String] = []
    /// Your other devices, by kind: "Mac and iPhone".
    var devices = "Mac and iPhone"
    /// The vaults a new machine can back up to.
    var vaults: [String] = []
    var open: () -> Void = {}
    var linkVault: () -> Void = {}
    var biometry = Biometry.name
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            switch approval.state {
            case .approved: approved
            default: asking
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .layer(.sheet)
        #if os(macOS)
        .frame(idealWidth: 400)
        #else
        .presentationDetents([.medium, .large])
        .presentationBackground(Theme.sheet)
        #endif
        .preferredColorScheme(.dark)
        .onChange(of: approval.state) { if approval.state == .declined { dismiss() } }
    }

    private var request: ApprovalRequest { approval.request }

    @ViewBuilder private var asking: some View {
        Text(eyebrow).font(.caption).foregroundStyle(Theme.tertiary)
        Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.text)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.top, 2)
        Text(about).font(Theme.body).foregroundStyle(Theme.secondary).padding(.top, 4)
        Text(request.spokenCode)
            .font(.system(size: 30, weight: .bold, design: .monospaced))
            .tracking(3.6)
            .foregroundStyle(Theme.text)
            .frame(maxWidth: .infinity)
            .padding(.top, 14)
            .accessibilityIdentifier("approval-code")
        Text("Check that the terminal shows the same code")
            .font(.caption).foregroundStyle(Theme.secondary)
            .frame(maxWidth: .infinity)
            .padding(.top, 4)
        VStack(alignment: .leading, spacing: 2) {
            Text("Fingerprint \(Text(request.shortFingerprint).font(Theme.monoSmall).foregroundStyle(Theme.secondary))")
            Text(consequence).fixedSize(horizontal: false, vertical: true)
        }
        .font(.footnote)
        .foregroundStyle(Theme.text)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 10)
        .padding(.horizontal, 12)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .padding(.vertical, 12)
        if case .failed(let why) = approval.state {
            Text(why).font(.footnote).foregroundStyle(Theme.danger).padding(.bottom, 8)
        }
        WideButton(title: "Approve with \(biometry)", primary: true) { Task { await approval.approve() } }
            .disabled(approval.state == .signing)
            .keyboardShortcut(.defaultAction)
            .accessibilityIdentifier("approve")
        WideButton(title: request.kind == .login ? "Not me" : "Not mine", tint: Theme.danger) {
            approval.decline()
        }
        .padding(.top, 8)
        .accessibilityIdentifier("decline")
    }

    @ViewBuilder private var approved: some View {
        Image(systemName: "face.smiling")
            .font(.system(size: 26))
            .foregroundStyle(Theme.action)
            .frame(width: 58, height: 58)
            .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(Theme.action, lineWidth: 2.5))
            .frame(maxWidth: .infinity)
            .padding(.top, 4)
            .padding(.bottom, 10)
        Text(doneTitle).font(.title3.weight(.bold)).foregroundStyle(Theme.text)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .accessibilityIdentifier("approved")
        Text(doneDetail).font(Theme.body).foregroundStyle(Theme.secondary)
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity)
            .padding(.top, 4)
            .padding(.bottom, 12)
        if request.kind == .machine, !vaults.isEmpty {
            Button(action: linkVault) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Next: \(Text("Link a vault").bold()) for backup and secrets").foregroundStyle(Theme.text)
                    Text(vaults.joined(separator: " · ")).foregroundStyle(Theme.secondary)
                }
                .font(.footnote)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 10)
                .padding(.horizontal, 12)
                .background(.card, in: .rect(cornerRadius: Theme.corner))
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .padding(.bottom, 12)
        }
        WideButton(title: request.kind == .login ? "Done" : "Open \(request.name)", primary: true) {
            if request.kind == .machine { open() }
            dismiss()
        }
        .keyboardShortcut(.defaultAction)
    }

    private var eyebrow: String {
        switch request.kind {
        case .machine: "NEW MACHINE"
        case .login: "TERMINAL SIGN-IN"
        }
    }

    private var title: String {
        switch request.kind {
        case .machine: "\(request.name) wants you as its owner"
        case .login: "A terminal on \(request.name) wants to sign in as you"
        }
    }

    private var about: String {
        switch request.kind {
        case .machine: request.about
        case .login: (["herder login"] + [request.about].filter { !$0.isEmpty }).joined(separator: " · ")
        }
    }

    private var consequence: String {
        switch request.kind {
        case .machine:
            teammates.isEmpty ? "It becomes yours; nobody else gets access unless you add them."
                : "It becomes yours; \(Directory.list(teammates)) get\(teammates.count == 1 ? "s" : "") access only if you add them."
        case .login:
            "It gets its own sign-in under you, listed with your devices. Your passkey stays on this device."
        }
    }

    private var doneTitle: String {
        switch request.kind {
        case .machine: "\(request.name) is yours"
        case .login: "The terminal on \(request.name) is signed in"
        }
    }

    private var doneDetail: String {
        switch request.kind {
        case .machine: "Your \(devices) can use it now: no code needed on them."
        case .login: "It's listed under your devices in People; sign it out there."
        }
    }
}

/// A sheet's full-width button: the action colour, or quiet with a hairline.
private struct WideButton: View {
    let title: String
    var primary = false
    var tint = Theme.text
    let action: () -> Void
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(Theme.body.weight(.semibold))
                .foregroundStyle(primary ? .white : tint)
                .frame(maxWidth: .infinity, minHeight: 42)
                .background(primary ? Theme.action : .clear, in: .rect(cornerRadius: 9))
                .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(primary ? Theme.action : Theme.stroke))
                .opacity(enabled ? 1 : 0.45)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

/// The approval a `herder://own` or `herder://login` link opens, signed with this device's
/// passkey and sent to the machine.
struct ApproveSheet: View {
    let fleet: Fleet
    @State private var approval: Approval
    let open: (HostId) -> Void
    let linkVault: () -> Void

    init(fleet: Fleet, request: ApprovalRequest, open: @escaping (HostId) -> Void, linkVault: @escaping () -> Void) {
        self.fleet = fleet
        self.open = open
        self.linkVault = linkVault
        _approval = State(initialValue: Approval(request, authenticator: PlatformAuthenticator()) {
            try await fleet.approve(request, with: $0)
        })
    }

    var body: some View {
        ApprovalSheet(approval: approval,
                      vaults: fleet.machines.filter { !$0.hosts.isEmpty }.map(\.name),
                      open: { if let machine = approval.machine { open(machine) } },
                      linkVault: linkVault)
    }
}
