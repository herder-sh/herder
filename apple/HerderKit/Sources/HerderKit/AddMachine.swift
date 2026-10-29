import Herder
import SwiftUI

/// What Add to Fleet adds: the first choice its sheet asks for.
enum PairKind: CaseIterable, Hashable {
    /// A machine running herder, which `herder claim` on it offers to you.
    case machine
    /// A vault: backups and secrets for the machines that link to it.
    case vault

    var title: String {
        switch self {
        case .machine: "A machine running herder"
        case .vault: "A vault"
        }
    }

    var detail: String {
        switch self {
        case .machine: "Runs your sessions"
        case .vault: "Backs up and keeps secrets"
        }
    }

    var symbol: String {
        switch self {
        case .machine: "desktopcomputer"
        case .vault: "lock.shield"
        }
    }

    /// What to run on a machine, or a vault's: it prints the `herder://own` link that makes
    /// you its owner, whether it has none yet or you own it from another device.
    static let command = "herder claim"
}

/// Adds to the fleet: a machine or a vault, which `herder claim` on it offers to you, or a
/// vault this device knows of, signed in to with your passkey. Opens on `kind`, with `vault`
/// chosen.
struct AddMachineSheet: View {
    let fleet: Fleet
    /// Hands the `herder://own` or `herder://login` link pasted or scanned to the approval, which
    /// takes the sheet's place.
    let approve: (ApprovalRequest) -> Void
    @State private var kind: PairKind
    @State private var vault: UnpairedVault?
    @State private var text: String
    @State private var vaults: [UnpairedVault] = []
    @State private var error: String?
    @State private var scanning = false
    @Environment(\.dismiss) private var dismiss

    init(fleet: Fleet, kind: PairKind, vault: UnpairedVault?, text: String = "",
         approve: @escaping (ApprovalRequest) -> Void) {
        self.fleet = fleet
        self.approve = approve
        _kind = State(initialValue: kind)
        _vault = State(initialValue: vault)
        _text = State(initialValue: text)
    }

    var body: some View {
        #if os(iOS)
        // The camera takes the sheet's place rather than opening over it.
        if scanning {
            // The approval shows what the code names, to check before Face ID.
            PairScanner(scanned: approve, close: { scanning = false })
                .presentationBackground(Theme.sheet)
        } else {
            form
        }
        #else
        form
        #endif
    }

    private var input: ApprovalRequest? { approvalRequest(in: text) }
    /// The known vault chosen, which you sign in to rather than claim.
    private var chosen: UnpairedVault? { kind == .vault ? vault : nil }

    private var form: some View {
        SheetScaffold(title: "Add to Fleet", height: 640, hint: hint) {
            AddMachineForm(kind: $kind, vault: $vault, text: $text, vaults: vaults, error: error,
                           paired: { fingerprint in fleet.machine(fingerprint: fingerprint)?.name },
                           scan: scan)
        } footer: {
            Spacer()
            if let chosen {
                ActionButton(title: "Sign In with \(Biometry.name)", style: .primary) { await signIn(to: chosen) }
                    .frame(width: 200)
                    .keyboardShortcut(.defaultAction)
            } else {
                ActionButton(title: "Continue", style: .primary) {
                    if let input { approve(input) }
                }
                .frame(width: 160)
                .disabled(input == nil)
                .keyboardShortcut(.defaultAction)
            }
        }
        .task { vaults = await fleet.unpairedVaults() }
    }

    private var hint: String {
        input != nil && chosen == nil ? "Next, check the code matches the terminal’s." : ""
    }

    private var scan: (() -> Void)? {
        #if os(iOS)
        { scanning = true }
        #else
        nil
        #endif
    }

    private func signIn(to vault: UnpairedVault) async {
        do {
            _ = try await fleet.signIn(hosts: [vault.address], fingerprint: vault.fingerprint,
                                       with: passkeyAuthenticator())
            dismiss()
        } catch {
            if !isCancelled(error) {
                self.error = "\(describe(error)). If \(vault.name) does not know you yet, run `\(PairKind.command)` on its machine and choose Another vault."
            }
        }
    }

}

/// Add to Fleet's choice and its steps, on bindings alone so they can be drawn without a fleet.
struct AddMachineForm: View {
    @Binding var kind: PairKind
    @Binding var vault: UnpairedVault?
    @Binding var text: String
    /// The vaults machines back up to that this device is not paired with.
    let vaults: [UnpairedVault]
    var error: String?
    /// The name this device knows a machine of that fingerprint by, if paired already.
    var paired: (String) -> String? = { _ in nil }
    /// Opens the camera, where there is one.
    var scan: (() -> Void)?

    private var input: ApprovalRequest? { approvalRequest(in: text) }

    var body: some View {
        KindPicker(kind: $kind)
        switch kind {
        case .machine:
            claimSteps(from: 1, on: "the machine")
        case .vault:
            if vaults.isEmpty {
                claimSteps(from: 1, on: "the vault’s machine")
            } else {
                NumberedStep(number: 1, title: "Which vault") { vaultPicker }
                if let vault {
                    NumberedStep(number: 2, title: "Sign in with your passkey") {
                        Text("\(vault.name) lets you in if you own it. To claim a vault nobody owns, choose Another vault.")
                            .font(.footnote).foregroundStyle(Theme.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                } else {
                    claimSteps(from: 2, on: "the vault’s machine")
                }
            }
        }
        if let error {
            Text(error).font(.footnote).foregroundStyle(Theme.failure)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    #if os(iOS)
    private let pasteTitle = "Scan the QR code it prints, or paste the link"
    #else
    private let pasteTitle = "Paste the link it prints"
    #endif

    /// Run `herder claim` on `machine`, paste or scan its link, check what it names.
    @ViewBuilder private func claimSteps(from number: Int, on machine: String) -> some View {
        NumberedStep(number: number, title: "On \(machine), run") { CommandBox(command: PairKind.command) }
        NumberedStep(number: number + 1, title: pasteTitle) { inputField }
        if let input {
            NumberedStep(number: number + 2, title: "Check, then approve with your passkey") {
                LinkCard(request: input, paired: paired)
            }
        }
    }

    private var inputField: some View {
        VStack(alignment: .leading, spacing: 8) {
            InputBox(placeholder: "herder://own?host=…&fp=…&code=…", text: $text, mono: true, lines: 1...4)
                .accessibilityIdentifier("pairing-link")
            HStack(spacing: 8) {
                if let scan {
                    PillButton(title: "Scan QR Code", symbol: "qrcode.viewfinder", prominent: true, action: scan)
                        .accessibilityIdentifier("scan-pairing-code")
                }
                PillButton(title: "Paste", symbol: "doc.on.clipboard") { text = Clipboard.string ?? text }
            }
            if input == nil && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Text("That is not the link `\(PairKind.command)` prints.")
                    .font(.footnote).foregroundStyle(Theme.failure)
            }
        }
    }

    private var vaultPicker: some View {
        Menu {
            ForEach(vaults) { vault in
                Button { self.vault = vault } label: {
                    Text(vault.name)
                    Text(vault.backsUp)
                }
            }
            Divider()
            Button("Another vault") { vault = nil }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: PairKind.vault.symbol).foregroundStyle(Theme.tertiary)
                Text(vault?.name ?? "Another vault").foregroundStyle(Theme.text).lineLimit(1)
                if let vault {
                    Text(vault.backsUp).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
                Spacer(minLength: 6)
                Image(systemName: "chevron.up.chevron.down").imageScale(.small).foregroundStyle(Theme.tertiary)
            }
            .font(.subheadline)
            .padding(.horizontal, 12)
            .frame(maxWidth: .infinity, minHeight: 36)
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            .contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .accessibilityIdentifier("pair-vault")
    }
}

/// The things Add to Fleet adds, as tiles side by side, or as rows where they do not fit.
private struct KindPicker: View {
    @Binding var kind: PairKind

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) {
                ForEach(PairKind.allCases, id: \.self) { tile($0, wide: false).frame(minWidth: 156) }
            }
            VStack(spacing: 6) {
                ForEach(PairKind.allCases, id: \.self) { tile($0, wide: true) }
            }
        }
    }

    private func tile(_ option: PairKind, wide: Bool) -> some View {
        let selected = option == kind
        return Button { kind = option } label: {
            let layout = wide ? AnyLayout(HStackLayout(spacing: 12)) : AnyLayout(VStackLayout(alignment: .leading, spacing: 8))
            layout {
                Image(systemName: option.symbol)
                    .font(.body)
                    .foregroundStyle(selected ? Theme.text : Theme.tertiary)
                    .frame(width: 22, height: 22, alignment: .leading)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(option.detail).font(.caption).foregroundStyle(Theme.secondary)
                }
                if wide { Spacer(minLength: 0) }
            }
            .padding(12)
            .frame(maxWidth: .infinity, maxHeight: wide ? nil : .infinity, alignment: wide ? .leading : .topLeading)
            .primaryFill(in: .rect(cornerRadius: Theme.corner), on: selected, off: AnyShapeStyle(.card))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(selected ? .clear : Theme.stroke))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// A command to run on the machine, with its copy button.
private struct CommandBox: View {
    let command: String

    var body: some View {
        HStack {
            Text(command).font(Theme.mono).foregroundStyle(Theme.text).textSelection(.enabled)
            Spacer()
            CopyButton(text: command)
        }
        .padding(.leading, 12)
        .padding(.trailing, 4)
        .frame(minHeight: 40)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// What a link names, to check before going on: the machine a `herder://own` link offers you,
/// or the one a `herder://login` link signs a terminal in to, with its addresses and fingerprint.
private struct LinkCard: View {
    let request: ApprovalRequest
    let paired: (String) -> String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: "person.badge.key")
                    .foregroundStyle(Theme.text)
                    .frame(width: 22, alignment: .leading)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        .accessibilityIdentifier("pair-grant")
                    Text(detail).font(.footnote).foregroundStyle(Theme.secondary)
                }
            }
            Rectangle().fill(Theme.stroke).frame(height: 1)
            VStack(alignment: .leading, spacing: 8) {
                DetailRow(label: "Addresses", value: request.hosts.joined(separator: "\n"), mono: true)
                DetailRow(label: "Fingerprint", value: grouped(request.fingerprint), mono: true)
                if request.kind == .machine, let name = paired(request.fingerprint) {
                    Text("Already in your fleet as \(name).")
                        .font(.footnote).foregroundStyle(Theme.secondary)
                }
            }
        }
        .padding(12)
        .fixedSize(horizontal: false, vertical: true)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }

    private var title: String {
        switch request.kind {
        case .machine: "You’ll own \(request.name)"
        case .login: "A terminal on \(request.name) signs in as you"
        }
    }

    private var detail: String {
        switch request.kind {
        case .machine: "Full control: sessions, terminals and secrets. Nobody else gets in until you add them."
        case .login: "It gets its own sign-in under you, listed with your devices."
        }
    }
}

/// A fingerprint in groups of four, easier to compare by eye.
func grouped(_ fingerprint: String) -> String {
    stride(from: 0, to: fingerprint.count, by: 4).map { start in
        let from = fingerprint.index(fingerprint.startIndex, offsetBy: start)
        return String(fingerprint[from..<(fingerprint.index(from, offsetBy: 4, limitedBy: fingerprint.endIndex) ?? fingerprint.endIndex)])
    }
    .joined(separator: " ")
}
