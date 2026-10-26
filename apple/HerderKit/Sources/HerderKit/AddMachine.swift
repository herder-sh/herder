import Herder
import SwiftUI

/// What Add Machine adds: the first choice its sheet asks for.
enum PairKind: CaseIterable, Hashable {
    /// A machine running herder, from the link `herder pair` prints on it.
    case machine
    /// A vault: backups and secrets for the machines that link to it.
    case vault
    /// Another device of the user's, with every machine this one has.
    case device

    var title: String {
        switch self {
        case .machine: "A machine running herder"
        case .vault: "A vault"
        case .device: "A device to this fleet"
        }
    }

    var detail: String {
        switch self {
        case .machine: "Runs your sessions"
        case .vault: "Backs up and keeps secrets"
        case .device: "Your phone or another Mac"
        }
    }

    var symbol: String {
        switch self {
        case .machine: "desktopcomputer"
        case .vault: "lock.shield"
        case .device: "qrcode"
        }
    }

    /// What to run on the machine for an owner pairing their own device: without `--role`, a
    /// daemon's second user would be a member.
    static let command = "herder pair --role owner"
}

/// What was pasted or scanned into Add Machine, as the link to pair with and what it grants.
struct PairInput: Equatable {
    let link: String
    let machines: [PairingUri]
    /// The role `herder pair` printed in the link; nil for a link another device shared,
    /// which pairs with that device's role on each machine.
    let role: Role?
    /// The machine's name, as `herder pair` printed it in the link.
    let name: String?

    /// `text` as a link to pair with: a `herder://pair` link anywhere in it; else the
    /// address, fingerprint and code lines `herder pair` prints; else, for a chosen vault, whose
    /// address and fingerprint are known, the bare code.
    init?(_ text: String, vault: UnpairedVault? = nil) {
        guard let link = pairingLink(in: text) ?? Self.fields(in: text, vault: vault).map({ pairingUriToString(uri: $0) }),
              let parsed = try? parsePairingLink(link: link) else { return nil }
        let items = URLComponents(string: link)?.queryItems ?? []
        self.link = link
        machines = parsed.machines
        role = switch items.first(where: { $0.name == "role" })?.value {
        case "owner": .owner
        case "member": .member
        default: nil
        }
        name = items.first { $0.name == "name" }?.value.flatMap { $0.isEmpty ? nil : $0 }
    }

    private static func fields(in text: String, vault: UnpairedVault?) -> PairingUri? {
        var hosts: [String] = []
        var fingerprint = vault?.fingerprint
        var code: String?
        for line in text.split(whereSeparator: \.isNewline) {
            let words = line.split(whereSeparator: \.isWhitespace).map(String.init)
            guard words.count == 2 else { continue }
            switch words[0].lowercased() {
            case "address": hosts.append(words[1])
            case "fingerprint": fingerprint = words[1]
            case "code": code = words[1]
            default: break
            }
        }
        let bare = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if code == nil, vault != nil, bare.wholeMatch(of: /[0-9A-Za-z]{5}-?[0-9A-Za-z]{5}/) != nil { code = bare }
        if hosts.isEmpty, let vault { hosts = [vault.address] }
        guard let fingerprint, let code, !hosts.isEmpty else { return nil }
        return PairingUri(hosts: hosts, fingerprint: fingerprint, code: code)
    }

    /// What pairing gets you, said before Pair: "You’ll join draugelis as owner", and what
    /// that role may do.
    func grant(vault: UnpairedVault?) -> (title: String, detail: String) {
        let name = name ?? vault?.name ?? (machines.count == 1 ? "this machine" : "\(machines.count) machines")
        return switch role {
        case .owner?: ("You’ll join \(name) as owner", "Full control: sessions, terminals and secrets.")
        case .member?: ("You’ll join \(name) as member", "Drives sessions; no terminals or secrets.")
        case nil: ("You’ll join \(name) as yourself", "With the role you have on each, as on the device that shared it.")
        }
    }
}

/// Adds to the fleet: a machine or a vault from what `herder pair` prints on it, or another
/// device of the user's from a link this one makes. Opens on `kind`, with `vault` chosen.
struct AddMachineSheet: View {
    let fleet: Fleet
    @State private var kind: PairKind
    @State private var vault: UnpairedVault?
    @State private var text = ""
    @State private var vaults: [UnpairedVault] = []
    @State private var error: String?
    @State private var scanning = false
    /// Each machine's result, once a link of several machines, or one that failed, paired.
    @State private var results: [PairResult]?
    @State private var shared: SharedLink?
    @State private var shareError: String?
    @Environment(\.dismiss) private var dismiss

    init(fleet: Fleet, kind: PairKind, vault: UnpairedVault?) {
        self.fleet = fleet
        _kind = State(initialValue: kind)
        _vault = State(initialValue: vault)
    }

    var body: some View {
        #if os(iOS)
        // The camera takes the sheet's place rather than opening over it.
        if scanning {
            PairScanner(scanned: { text = $0 }, close: { scanning = false })
                .presentationBackground(.black)
        } else {
            form
        }
        #else
        form
        #endif
    }

    private var input: PairInput? { PairInput(text, vault: kind == .vault ? vault : nil) }

    private var form: some View {
        SheetScaffold(title: "Add Machine", height: 640, hint: hint) {
            if let results {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(Array(results.enumerated()), id: \.offset) { PairResultRow(result: $0.element) }
                }
            } else {
                AddMachineForm(kind: $kind, vault: $vault, text: $text, vaults: vaults, error: error,
                               paired: { fingerprint in fleet.machines.first { $0.fingerprint == fingerprint }?.name },
                               scan: scan) {
                    if let shared {
                        SharedLinkView(fleet: fleet, shared: shared)
                    } else if let shareError {
                        Text(shareError).font(.footnote).foregroundStyle(Theme.failure)
                    } else {
                        HStack(spacing: 10) {
                            ProgressView().controlSize(.small)
                            Text("Asking each machine for a code…").foregroundStyle(Theme.secondary)
                        }
                        .font(.subheadline)
                        .frame(maxWidth: .infinity, minHeight: 200)
                    }
                }
            }
        } footer: {
            Spacer()
            if results != nil {
                ActionButton(title: "Done", style: .primary) { dismiss() }
                    .frame(width: 160)
                    .keyboardShortcut(.defaultAction)
            } else if kind == .device {
                ActionButton(title: "New Link", style: .secondary) { await share() }
                    .frame(width: 140)
                ActionButton(title: "Done", style: .primary) { dismiss() }
                    .frame(width: 140)
                    .keyboardShortcut(.defaultAction)
            } else {
                ActionButton(title: "Pair", style: .primary) { await pair() }
                    .frame(width: 160)
                    .disabled(AddMachineForm.blocked(input, vault: kind == .vault ? vault : nil))
                    .keyboardShortcut(.defaultAction)
            }
        }
        .task { vaults = await fleet.unpairedVaults() }
        .task(id: kind) {
            if kind == .device && shared == nil { await share() }
        }
    }

    private var hint: String {
        switch kind {
        case .machine, .vault: input != nil && results == nil ? "The fingerprint must match the one `herder pair` printed." : ""
        default: ""
        }
    }

    private var scan: (() -> Void)? {
        #if os(iOS)
        { scanning = true }
        #else
        nil
        #endif
    }

    private func pair() async {
        guard let input else { return }
        do {
            let results = try await fleet.pair(link: input.link)
            // One machine that paired needs no report: it shows in the list.
            if results.count == 1, case .paired = results[0] {
                dismiss()
            } else {
                self.results = results
            }
        } catch {
            self.error = describe(error)
        }
    }

    private func share() async {
        do {
            shared = try await fleet.share()
            shareError = nil
        } catch HerderError.Pairing(let detail) {
            shared = nil
            shareError = "Cannot share: \(detail)."
        } catch {
            shared = nil
            shareError = describe(error)
        }
    }
}

/// Add Machine's choice and its steps, on bindings alone so they can be drawn without a fleet;
/// `device` is what the device choice shows.
struct AddMachineForm<Device: View>: View {
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
    @ViewBuilder var device: Device

    /// Whether Pair cannot go ahead: nothing to pair with yet, or a link of another machine
    /// than the vault chosen.
    static func blocked(_ input: PairInput?, vault: UnpairedVault?) -> Bool {
        guard let input else { return true }
        return vault.map { vault in input.machines.contains { $0.fingerprint != vault.fingerprint } } ?? false
    }

    private var chosen: UnpairedVault? { kind == .vault ? vault : nil }
    private var input: PairInput? { PairInput(text, vault: chosen) }

    var body: some View {
        KindPicker(kind: $kind)
        switch kind {
        case .device:
            device
        case .machine:
            NumberedStep(number: 1, title: "On the machine, run") { CommandBox(command: PairKind.command) }
            NumberedStep(number: 2, title: pasteTitle) { inputField }
            check(number: 3)
        case .vault:
            if vaults.isEmpty {
                NumberedStep(number: 1, title: "On the vault’s machine, run") { CommandBox(command: PairKind.command) }
                NumberedStep(number: 2, title: pasteTitle) { inputField }
                check(number: 3)
            } else {
                NumberedStep(number: 1, title: "Which vault") { vaultPicker }
                NumberedStep(number: 2, title: vault.map { "On \($0.machine ?? $0.name), run" } ?? "On the vault’s machine, run") {
                    CommandBox(command: PairKind.command)
                }
                NumberedStep(number: 3, title: vault == nil ? pasteTitle : "Enter the code it prints") { inputField }
                check(number: 4)
            }
        }
        if let error {
            Text(error).font(.footnote).foregroundStyle(Theme.failure)
        }
    }

    #if os(iOS)
    private let pasteTitle = "Scan the QR code it prints, or paste the link"
    #else
    private let pasteTitle = "Paste the link it prints"
    #endif

    private var inputField: some View {
        VStack(alignment: .leading, spacing: 8) {
            InputBox(placeholder: chosen == nil ? "herder://pair?host=…&fp=…&code=…" : "ABCDE-FGHJK, or the link",
                     text: $text, mono: true, lines: 1...4)
                .accessibilityIdentifier("pairing-link")
            HStack(spacing: 8) {
                if let scan {
                    PillButton(title: "Scan QR Code", symbol: "qrcode.viewfinder", prominent: true, action: scan)
                        .accessibilityIdentifier("scan-pairing-code")
                }
                PillButton(title: "Paste", symbol: "doc.on.clipboard") { text = Clipboard.string ?? text }
            }
            if input == nil && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Text(chosen == nil ? "That is not a herder pairing link." : "That is neither a pairing code nor a link.")
                    .font(.footnote).foregroundStyle(Theme.failure)
            }
        }
    }

    @ViewBuilder private func check(number: Int) -> some View {
        if let input {
            NumberedStep(number: number, title: input.machines.count == 1 ? "Check, then pair" : "Check these \(input.machines.count) machines, then pair") {
                GrantCard(input: input, vault: chosen, paired: paired)
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
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            .contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .accessibilityIdentifier("pair-vault")
    }
}

/// The three things Add Machine adds, as tiles side by side, or as rows where they do not fit.
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
                    .frame(width: 22, alignment: .leading)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(option.detail).font(.caption).foregroundStyle(Theme.secondary)
                }
                if wide { Spacer(minLength: 0) }
            }
            .padding(12)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .primaryFill(in: .rect(cornerRadius: Theme.corner), on: selected, off: Theme.background)
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
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// What pairing gets you and with what: the role, then each machine's addresses and
/// fingerprint to check against what `herder pair` printed.
private struct GrantCard: View {
    let input: PairInput
    let vault: UnpairedVault?
    let paired: (String) -> String?

    var body: some View {
        let grant = input.grant(vault: vault)
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: input.role == .member ? "person" : "person.badge.key")
                    .foregroundStyle(Theme.text)
                    .frame(width: 22, alignment: .leading)
                VStack(alignment: .leading, spacing: 2) {
                    Text(grant.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        .accessibilityIdentifier("pair-grant")
                    Text(grant.detail).font(.footnote).foregroundStyle(Theme.secondary)
                    if input.role == .member {
                        Text("To join as owner, run `\(PairKind.command)` there instead.")
                            .font(.footnote).foregroundStyle(Theme.tertiary)
                    }
                }
            }
            if let vault, input.machines.contains(where: { $0.fingerprint != vault.fingerprint }) {
                Text("This code is from another machine than \(vault.name).")
                    .font(.footnote).foregroundStyle(Theme.failure)
            }
            ForEach(Array(input.machines.enumerated()), id: \.offset) { _, uri in
                Rectangle().fill(Theme.stroke).frame(height: 1)
                VStack(alignment: .leading, spacing: 8) {
                    DetailRow(label: "Addresses", value: uri.hosts.joined(separator: "\n"), mono: true)
                    DetailRow(label: "Fingerprint", value: grouped(uri.fingerprint), mono: true)
                    if let name = paired(uri.fingerprint) {
                        Text("Already paired as \(name): pairing again gives it a new key.")
                            .font(.footnote).foregroundStyle(Theme.secondary)
                    }
                }
            }
        }
        .padding(12)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// One machine of a link: paired, or why not.
private struct PairResultRow: View {
    let result: PairResult

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            switch result {
            case .paired(let machine):
                Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.success)
                VStack(alignment: .leading, spacing: 2) {
                    Text(machine.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("Paired").font(.caption).foregroundStyle(Theme.secondary)
                }
            case .failed(let addresses, let error):
                Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.failure)
                VStack(alignment: .leading, spacing: 2) {
                    Text(addresses.first ?? "A machine").font(Theme.mono).foregroundStyle(Theme.text)
                    Text("Not paired: \(error)").font(.caption).foregroundStyle(Theme.failure)
                }
            }
            Spacer(minLength: 0)
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
