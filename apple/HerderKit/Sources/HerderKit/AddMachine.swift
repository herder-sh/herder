import Herder
import SwiftUI
#if os(macOS)
import AppKit
#endif

/// How a new machine starts: Add a machine's three tabs.
enum MachineSource: CaseIterable, Hashable {
    /// A Linux box or a Mac, with the one-line install.
    case install
    /// A vault in a Kubernetes cluster, from `deploy/vault.yaml`.
    case kubernetes
    /// A machine that runs herder already: `herder claim` there.
    case hasHerder

    var title: String {
        switch self {
        case .install: "Linux or Mac"
        case .kubernetes: "Kubernetes"
        case .hasHerder: "Has herder"
        }
    }

    /// Where to run `command`.
    var runOn: String {
        switch self {
        case .install: "On the new machine, run"
        case .kubernetes: "Where kubectl reaches your cluster, run"
        case .hasHerder: "On the machine, run"
        }
    }

    /// What prints the `herder://own` link that makes you the machine's owner.
    var command: String {
        switch self {
        case .install:
            "curl -fsSL https://herder.sh/install | sh\nherder service install"
        case .kubernetes:
            "kubectl apply -n herder -f https://raw.githubusercontent.com/herder-sh/herder/main/deploy/vault.yaml\nkubectl exec -it -n herder herder-vault-0 -- /herder claim"
        case .hasHerder:
            Self.claimCommand
        }
    }

    /// The line under the command: what it does.
    var note: String {
        switch self {
        case .install: "It installs herder, starts it at every boot, and prints a link and a QR code."
        case .kubernetes: "It runs a vault, which backs up your machines and keeps their secrets. Create its Secrets first, as deploy/README.md says."
        case .hasHerder: "It prints a link and a QR code that make you its owner, whether it has none yet or you own it from another device."
        }
    }

    /// What to run on a machine, or a vault's: it prints the `herder://own` link that makes
    /// you its owner, whether it has none yet or you own it from another device.
    static let claimCommand = "herder claim"

    /// Add a machine's progress.
    static let steps = ["Install", "Approve", "Done"]
}

/// What Add a machine adds: a machine whose `herder://own` link was scanned or pasted, or a vault
/// your machines back up to that this device has not signed in to.
enum AddMachineTarget: Hashable {
    case machine(ApprovalRequest)
    case vault(UnpairedVault)

    var name: String {
        switch self {
        case .machine(let request): request.name
        case .vault(let vault): vault.name
        }
    }
}

/// Where Add a machine is: the command to run, the machine to approve, or done.
enum AddMachineStep: Hashable {
    case install
    case approve(AddMachineTarget)
    /// Approved: the machine's host id here.
    case done(AddMachineTarget, HostId)

    /// Its place in `MachineSource.steps`; past the last once done.
    var index: Int {
        switch self {
        case .install: 0
        case .approve: 1
        case .done: MachineSource.steps.count
        }
    }
}

/// Who approves, as the approve step's card says it: you as your machines know you, with where
/// your passkey is kept, so you know which one Face ID will ask for.
struct Approver: Equatable {
    var name: String
    /// "Your passkey in Apple Passwords".
    var detail: String

    /// Before your machines have said who you are.
    static let unknown = Approver(name: "You", detail: "Your herder passkey")

    /// `person`, as your machines list you: their name, and where their passkeys are kept.
    static func of(_ person: PersonInfo?) -> Approver {
        guard let person else { return unknown }
        var homes: [String] = []
        for home in person.passkeys.compactMap({ PasskeyHome.name(aaguid: $0.aaguid) }) where !homes.contains(home) {
            homes.append(home)
        }
        let detail = switch homes.count {
        case 0: "Your herder passkey"
        case 1: "Your passkey in \(homes[0])"
        default: "Your passkey in \(homes.dropLast().joined(separator: ", ")) or \(homes.last ?? "")"
        }
        return Approver(name: person.name, detail: detail)
    }
}

/// Add a machine's offer to back the new machine up to a vault you own.
struct BackupOffer: Equatable {
    let vault: String
    var on = true
    var saved: SaveState?
}

/// Add a machine: one sheet that runs Install, Approve, Done. Install has three tabs (a
/// Linux or Mac box, Kubernetes, a machine that has herder) with the command that prints the
/// machine's `herder://own` link; the Mac watches the clipboard for it, the iPhone scans its QR
/// code. Approve shows the machine's name, the code its terminal shows and the passkey that
/// approves. Done lists your other devices and backs the machine up to a vault you own. A vault
/// your machines back up to is signed in to the same way. Any other herder link (a terminal's
/// `herder://login`, another device's `herder://fleet`) hands over to its own sheet.
struct AddMachineSheet: View {
    let fleet: Fleet
    /// Replaces this sheet with another: the one a `herder://login` or `herder://fleet` link
    /// takes, or Add a Device.
    let present: (AppSheet) -> Void
    /// Starts a session on the new machine.
    let drafted: (Draft) -> Void
    @State private var source: MachineSource
    @State private var step: AddMachineStep
    @State private var text = ""
    @State private var vaults: [UnpairedVault] = []
    @State private var people: FleetPeople
    @State private var newName: String?
    @State private var error: String?
    @State private var scanning = false
    @State private var backup: BackupOffer?
    /// The new machine is linked to the offered vault, so the switch sets it rather than links.
    @State private var linked = false
    @Environment(\.dismiss) private var dismiss

    init(fleet: Fleet, source: MachineSource = .install, vault: UnpairedVault? = nil, scanning: Bool = false,
         present: @escaping (AppSheet) -> Void, drafted: @escaping (Draft) -> Void) {
        self.fleet = fleet
        self.present = present
        self.drafted = drafted
        _source = State(initialValue: source)
        _step = State(initialValue: vault.map { .approve(.vault($0)) } ?? .install)
        _people = State(initialValue: FleetPeople(fleet: fleet))
        #if os(iOS)
        _scanning = State(initialValue: scanning)
        #endif
    }

    var body: some View {
        #if os(iOS)
        // The camera takes the sheet's place rather than opening over it.
        if scanning {
            PairScanner(scanned: { link in
                scanning = false
                take(link)
            }, close: { scanning = false })
            .presentationBackground(Theme.sheet)
        } else {
            sheet
        }
        #else
        sheet
            .task(id: step == .install) { await watchClipboard() }
        #endif
    }

    private var sheet: some View {
        AddMachineView(source: $source, text: $text, newName: $newName, step: step,
                       approver: approver, offersNewPasskey: fleet.machines.isEmpty,
                       paired: { fleet.machine(fingerprint: $0)?.name }, vaults: vaults,
                       devices: devices, backup: backup, error: error, actions: actions)
            .task { await people.reload(fleet.machines) }
            .task { vaults = await fleet.unpairedVaults() }
            .task(id: step) { await settle() }
    }

    private var actions: AddMachineView.Actions {
        var actions = AddMachineView.Actions()
        #if os(iOS)
        actions.scan = { scanning = true }
        #endif
        actions.paste = {
            text = Clipboard.string ?? ""
            if let link = pairLink(in: text) { take(link) }
        }
        actions.go = { if let link = pairLink(in: text) { take(link) } }
        actions.approve = { await approve() }
        actions.decline = {
            text = ""
            error = nil
            step = .install
        }
        actions.signIn = { vault in
            error = nil
            step = .approve(.vault(vault))
        }
        actions.addDevice = !fleet.canShare ? nil : { present(.addDevice) }
        if case .done(.machine, let hostId) = step, runsSessions(hostId) {
            actions.startSession = {
                drafted(Draft(hostId: hostId))
                dismiss()
            }
        }
        actions.finish = { dismiss() }
        actions.setBackup = { setBackup($0) }
        return actions
    }

    /// You as the machines know you, once they have said.
    private var approver: Approver {
        .of(Fleet.keyed(FleetPeople.me(fleet.machines), in: people.people))
    }

    /// Your devices: this one first, then the others your machines list.
    private var devices: [Directory.Device] {
        #if os(macOS)
        let this = Directory.Device(id: FleetPeople.thisClient, kind: .mac, current: true)
        #else
        let this = Directory.Device(id: FleetPeople.thisClient, kind: .phone, current: true)
        #endif
        return [this] + (people.directory(fleet.machines)?.devices.filter { !$0.current } ?? [])
    }

    /// Whether `hostId` runs sessions, rather than being a vault.
    private func runsSessions(_ hostId: HostId) -> Bool {
        fleet.machines.first { $0.hostId == hostId }.map { $0.hosts.isEmpty && $0.vault == nil } ?? true
    }

    /// A link scanned, pasted or copied: a machine's `herder://own` goes to Approve; any other
    /// link to the sheet that takes it, in this one's place.
    private func take(_ link: PairLink) {
        switch link {
        case .approve(let request) where request.kind == .machine:
            error = nil
            newName = nil
            step = .approve(.machine(request))
        default:
            present(link.sheet)
        }
    }

    private func approve() async {
        error = nil
        switch step {
        case .approve(let target):
            do {
                let hostId: HostId
                switch target {
                case .machine(let request):
                    let name = newName?.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard let approved = try await fleet.approve(request, with: passkeyAuthenticator(),
                                                                 newPasskey: name?.isEmpty == false ? name : nil)
                    else { return }
                    hostId = approved
                case .vault(let vault):
                    hostId = try await fleet.signIn(hosts: [vault.address], fingerprint: vault.fingerprint,
                                                    with: passkeyAuthenticator())
                }
                step = .done(target, hostId)
            } catch {
                guard !isCancelled(error) else { return }
                if case .vault(let vault) = target {
                    self.error = "\(describe(error)). If \(vault.name) does not know you yet, run `\(MachineSource.claimCommand)` on its machine, then add it from Has herder."
                } else {
                    self.error = describe(error)
                }
            }
        default:
            return
        }
    }

    /// Once done, asks the machines for your devices again, and offers a vault you own to back
    /// a new machine up to, linking it at once, unless it backs up somewhere already.
    private func settle() async {
        guard case .done(let target, let hostId) = step else { return }
        await people.reload(fleet.machines)
        guard case .machine = target, runsSessions(hostId), let vault = fleet.backupVaults(for: hostId).first,
              case .nowhere? = try? await fleet.backupLink(of: hostId)
        else { return }
        backup = BackupOffer(vault: vault.name)
        setBackup(true)
    }

    private func setBackup(_ on: Bool) {
        guard case .done(_, let hostId) = step, let host = fleet.machines.first(where: { $0.hostId == hostId }),
              let vault = fleet.backupVaults(for: hostId).first
        else { return }
        backup?.on = on
        backup?.saved = .saving
        Task {
            do {
                if linked {
                    try await fleet.setVaultLink(hostId, backup: on, secrets: on)
                } else if on {
                    try await fleet.link(host, to: vault, backup: true, secrets: true)
                    linked = true
                }
                backup?.saved = .saved
            } catch {
                backup?.on = linked && !on
                backup?.saved = .failed(describe(error))
            }
        }
    }

    #if os(macOS)
    /// While on Install, takes a herder link as soon as it is copied, from the terminal that
    /// printed it.
    private func watchClipboard() async {
        guard step == .install else { return }
        var seen = NSPasteboard.general.changeCount
        while !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(400))
            let count = NSPasteboard.general.changeCount
            guard count != seen else { continue }
            seen = count
            if let copied = Clipboard.string, let link = pairLink(in: copied) {
                text = copied
                take(link)
                return
            }
        }
    }
    #endif
}

/// Add a machine's steps on bindings and values alone, so they can be drawn without a fleet.
struct AddMachineView: View {
    /// What its buttons do; nil hides a button.
    struct Actions {
        /// Opens the camera, where there is one.
        var scan: (() -> Void)?
        var paste: () -> Void = {}
        /// Goes on with the link typed or pasted.
        var go: () -> Void = {}
        var approve: () async -> Void = {}
        var decline: () -> Void = {}
        var signIn: (UnpairedVault) -> Void = { _ in }
        /// Opens Add a Device, to bring the machine to another device of yours.
        var addDevice: (() -> Void)?
        var startSession: (() -> Void)?
        var finish: () -> Void = {}
        var setBackup: (Bool) -> Void = { _ in }
    }

    @Binding var source: MachineSource
    @Binding var text: String
    /// The name of the passkey to make now; nil to approve with the one you have.
    @Binding var newName: String?
    let step: AddMachineStep
    var approver = Approver.unknown
    /// On a device with no machines yet, someone new to herder can make a passkey here.
    var offersNewPasskey = false
    /// The name this device knows a machine of that fingerprint by, if added already.
    var paired: (String) -> String? = { _ in nil }
    /// Vaults your machines back up to that this device has not signed in to.
    var vaults: [UnpairedVault] = []
    var devices: [Directory.Device] = []
    var backup: BackupOffer?
    var error: String?
    var actions = Actions()
    /// The sheet's height on the Mac.
    var height: CGFloat = 660

    #if os(iOS)
    private static let sheetTitle = "Add a Machine"
    private static let installHint = ""
    #else
    private static let sheetTitle = "Add a machine"
    private static let installHint = "Machines on Tailscale need Tailscale on this Mac too."
    #endif

    var body: some View {
        SheetScaffold(title: title, subtitle: subtitle, height: height, hint: hint) {
            FlowProgress(steps: MachineSource.steps, current: step.index)
            switch step {
            case .install: install
            case .approve(let target): approving(target)
            case .done: done
            }
            if let error {
                Text(LocalizedStringKey(error)).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("add-machine-error")
            }
        } footer: {
            footer
        }
    }

    private var title: String {
        guard case .done(let target, _) = step else { return Self.sheetTitle }
        switch target {
        case .machine: return "\(target.name) is yours"
        case .vault: return "Signed in to \(target.name)"
        }
    }

    private var subtitle: String {
        guard case .done(let target, _) = step else { return "" }
        switch target {
        case .machine: return "You’re its owner."
        case .vault: return "It knows you by your passkey."
        }
    }

    private var hint: String {
        switch step {
        case .install: Self.installHint
        case .approve(.machine): "You become its owner."
        case .approve(.vault): "It lets you in if you own it."
        case .done: ""
        }
    }

    private var link: PairLink? { pairLink(in: text) }

    // MARK: Install

    @ViewBuilder private var install: some View {
        SourcePicker(source: $source)
        #if os(macOS)
        NumberedStep(number: 1, title: source.runOn) { command }
        NumberedStep(number: 2, title: "Copy the link it prints") { watchField }
        #else
        VStack(alignment: .leading, spacing: 10) {
            Text(source.runOn).font(.subheadline).foregroundStyle(Theme.secondary)
            command
            note("It prints a QR code. Scan it here.", symbol: "qrcode")
            note("Only reachable over Tailscale? Turn Tailscale on here first.", symbol: "network")
            if !text.isEmpty && link == nil { notALink }
        }
        #endif
        if !vaults.isEmpty { vaultList }
    }

    private var command: some View {
        VStack(alignment: .leading, spacing: 8) {
            CommandBox(command: source.command)
            Text(LocalizedStringKey(source.note)).font(.footnote).foregroundStyle(Theme.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The Mac's: the clipboard is watched for the link, which can be pasted here too.
    private var watchField: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                if text.isEmpty { ProgressView().controlSize(.small) }
                TextField("Watching the clipboard for a herder link…", text: $text)
                    .textFieldStyle(.plain)
                    .font(text.isEmpty ? .subheadline : Theme.mono)
                    .foregroundStyle(Theme.text)
                    .autocorrectionDisabled()
                    .onSubmit(actions.go)
                    .accessibilityIdentifier("pairing-link")
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && link == nil { notALink }
        }
    }

    private var notALink: some View {
        Text("That is not a herder link. Copy the whole `herder://own…` line the command printed.")
            .font(.footnote).foregroundStyle(Theme.failure)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func note(_ text: String, symbol: String) -> some View {
        Label {
            Text(text).fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol).foregroundStyle(Theme.tertiary)
        }
        .font(.subheadline)
        .foregroundStyle(Theme.secondary)
    }

    /// The vaults your machines back up to that this device has not signed in to: a vault is
    /// added as a machine is, and one that knows you already lets you in with your passkey.
    private var vaultList: some View {
        SettingsList(title: "Or a vault your machines use") {
            ForEach(Array(vaults.enumerated()), id: \.element.id) { index, vault in
                if index > 0 { RowDivider() }
                SettingRow(label: vault.name, hint: vault.backsUp) {
                    PillButton(title: "Sign In", action: { actions.signIn(vault) })
                        .accessibilityIdentifier("sign-in-vault")
                }
            }
        }
    }

    // MARK: Approve

    @ViewBuilder private func approving(_ target: AddMachineTarget) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(heading(target)).font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("add-machine-heading")
            TargetCard(target: target)
            if case .machine(let request) = target, let name = paired(request.fingerprint) {
                Text("Already in your fleet as \(name).").font(.footnote).foregroundStyle(Theme.secondary)
            }
        }
        VStack(alignment: .leading, spacing: 10) {
            Text("As").font(.subheadline).foregroundStyle(Theme.secondary)
            if let name = newName {
                newPasskey(name)
            } else {
                ApproverCard(approver: approver)
                if offersNewPasskey, case .machine = target {
                    Button("New to herder? Create a passkey") { newName = Fleet.deviceUserName }
                        .font(.footnote)
                        .buttonStyle(.plain)
                        .foregroundStyle(Theme.action)
                }
            }
        }
    }

    private func heading(_ target: AddMachineTarget) -> String {
        switch target {
        case .machine: "Make \(target.name) yours"
        case .vault: "Sign in to \(target.name)"
        }
    }

    /// Someone new to herder: their name, which the passkey made now is saved under.
    @ViewBuilder private func newPasskey(_ name: String) -> some View {
        InputBox(placeholder: "Your name", text: Binding(get: { name }, set: { newName = $0 }))
            .accessibilityIdentifier("new-passkey-name")
        Text("A new passkey, saved in iCloud Keychain or your password manager. Anyone who uses herder already approves with theirs.")
            .font(.footnote).foregroundStyle(Theme.tertiary)
            .fixedSize(horizontal: false, vertical: true)
        Button("I have a herder passkey") { newName = nil }
            .font(.footnote)
            .buttonStyle(.plain)
            .foregroundStyle(Theme.action)
    }

    // MARK: Done

    @ViewBuilder private var done: some View {
        SettingsList(title: "Your devices") {
            ForEach(Array(devices.enumerated()), id: \.element.id) { index, device in
                if index > 0 { RowDivider() }
                deviceRow(device)
            }
        }
        if let backup {
            SettingsList(title: "Backup") {
                SettingRow(label: "Back up to \(backup.vault)", hint: "Sessions and secrets", saved: backup.saved) {
                    Toggle("Back up to \(backup.vault)", isOn: Binding(get: { backup.on }, set: actions.setBackup))
                        .labelsHidden()
                        .toggleStyle(.settings)
                        .accessibilityIdentifier("back-up-new-machine")
                }
            }
        }
    }

    private func deviceRow(_ device: Directory.Device) -> some View {
        HStack(spacing: 12) {
            Group {
                if device.current {
                    Image(systemName: "checkmark").font(.subheadline.weight(.heavy)).foregroundStyle(Theme.switchOn)
                } else if device.kind == .terminal {
                    Text("–").font(.subheadline).foregroundStyle(Theme.tertiary)
                } else {
                    Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 1.5, dash: [2.5, 2.5]))
                        .frame(width: 16, height: 16)
                }
            }
            .frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(device.title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                Text(Self.coverage(device)).font(.caption).foregroundStyle(Theme.secondary)
            }
            Spacer(minLength: 8)
            if !device.current, device.kind != .terminal, let addDevice = actions.addDevice {
                PillButton(title: "Add a Device", action: addDevice)
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 8)
        .frame(minHeight: Theme.tallRowHeight)
    }

    /// The line under a device on Done: whether it can use the new machine yet.
    static func coverage(_ device: Directory.Device) -> String {
        if device.current { return "Signed in" }
        if device.kind == .terminal { return "Not needed" }
        return "Not on it yet: Add a Device brings it over"
    }

    // MARK: Footer

    @ViewBuilder private var footer: some View {
        switch step {
        case .install:
            #if os(iOS)
            VStack(spacing: 6) {
                if let scan = actions.scan {
                    FlowButton(title: "Scan the Code", symbol: "camera", forward: true, action: scan)
                        .accessibilityIdentifier("scan-pairing-code")
                }
                FlowButton(title: "Paste a Link", plain: true, action: actions.paste)
                    .accessibilityIdentifier("pairing-link")
            }
            #else
            Spacer()
            FlowButton(title: "Continue", forward: true, action: actions.go)
                .disabled(link == nil)
                .keyboardShortcut(.defaultAction)
            #endif
        case .approve(let target):
            gap
            if case .machine = target {
                FlowButton(title: "Not Mine", action: actions.decline)
                    .accessibilityIdentifier("decline")
            }
            FlowButton(title: approveTitle(target), symbol: Biometry.name == "Face ID" ? "faceid" : "touchid", forward: true, run: actions.approve)
                .disabled(newName?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true)
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("approve")
        case .done:
            gap
            if let startSession = actions.startSession {
                FlowButton(title: "Start a Session on It", action: startSession)
            }
            FlowButton(title: "Done", forward: true, action: actions.finish)
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("add-machine-done")
        }
    }

    /// Pushes the Mac's footer buttons to the right; the iPhone's fill the width.
    @ViewBuilder private var gap: some View {
        #if os(macOS)
        Spacer()
        #endif
    }

    private func approveTitle(_ target: AddMachineTarget) -> String {
        if newName != nil { return "Create Passkey and Approve" }
        switch target {
        case .machine: return "Approve with \(Biometry.name)"
        case .vault: return "Sign In with \(Biometry.name)"
        }
    }
}

/// Install's tabs, as one segmented capsule.
private struct SourcePicker: View {
    @Binding var source: MachineSource

    var body: some View {
        HStack(spacing: 2) {
            ForEach(MachineSource.allCases, id: \.self) { option in
                let selected = option == source
                Button { source = option } label: {
                    Text(option.title)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(selected ? Theme.text : Theme.secondary)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, minHeight: 32)
                        .background(selected ? Theme.raised : .clear, in: .capsule)
                        .contentShape(.capsule)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
        .padding(3)
        .background(.card, in: .capsule)
        .overlay(Capsule().strokeBorder(Theme.stroke.opacity(0.6)))
        #if os(macOS)
        .frame(maxWidth: 420)
        #endif
    }
}

/// A command to run, on as many lines as it has, with its copy button.
private struct CommandBox: View {
    let command: String

    var body: some View {
        HStack(alignment: .top) {
            Text(command)
                .font(Theme.mono)
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 12)
            CopyButton(text: command).padding(.top, 5)
        }
        .padding(.leading, 14)
        .padding(.trailing, 6)
        .background(.ground, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// What the link names, recognised by name: the machine, where it is, and the code its
/// terminal shows to compare; or the vault, and who backs up to it.
private struct TargetCard: View {
    let target: AddMachineTarget

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.body)
                .foregroundStyle(Theme.secondary)
                .frame(width: 36, height: 36)
                .background(Theme.raised, in: .rect(cornerRadius: 8))
            VStack(alignment: .leading, spacing: 2) {
                Text(target.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    .accessibilityIdentifier("pair-grant")
                Text(detail).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(2)
            }
            Spacer(minLength: 8)
            if case .machine(let request) = target {
                VStack(alignment: .trailing, spacing: 2) {
                    Text("Terminal shows").font(.caption).foregroundStyle(Theme.tertiary)
                    Text(request.spokenCode)
                        .font(.system(.subheadline, design: .monospaced).weight(.bold))
                        .tracking(1)
                        .foregroundStyle(Theme.text)
                        .accessibilityIdentifier("approval-code")
                }
                .fixedSize()
            }
        }
        .padding(12)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }

    private var symbol: String {
        switch target {
        case .machine: "server.rack"
        case .vault: "lock.shield"
        }
    }

    private var detail: String {
        switch target {
        case .machine(let request): "\(request.about) · \(request.shortFingerprint)"
        case .vault(let vault): "\(ApprovalRequest.address(vault.address)) · \(ApprovalRequest.short(vault.fingerprint))"
        }
    }
}

/// The passkey that approves: whose, and where it is kept.
private struct ApproverCard: View {
    let approver: Approver

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "key.fill")
                .font(.body)
                .foregroundStyle(Theme.action)
                .frame(width: 36, height: 36)
                .background(Theme.action.opacity(0.15), in: .circle)
            VStack(alignment: .leading, spacing: 2) {
                Text(approver.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    .accessibilityIdentifier("approver-name")
                Text(approver.detail).font(.caption).foregroundStyle(Theme.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// A footer button of a guided flow: the one forward step filled blue, the others raised or,
/// `plain`, text alone. Full width on the iPhone, its title's width on the Mac. With `run`,
/// a spinner while it runs.
private struct FlowButton: View {
    let title: String
    var symbol: String?
    var forward = false
    var plain = false
    var action: () -> Void = {}
    var run: (() async -> Void)?
    @State private var running = false
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button {
            guard let run else { return action() }
            running = true
            Task {
                await run()
                running = false
            }
        } label: {
            HStack(spacing: 6) {
                if running {
                    ProgressView().controlSize(.small).tint(forward ? .white : Theme.text)
                } else if let symbol {
                    Image(systemName: symbol)
                }
                Text(title)
            }
            .font(.body.weight(.semibold))
            .foregroundStyle(forward ? .white : plain ? Theme.action : Theme.text)
            .padding(.horizontal, 16)
            #if os(iOS)
            .frame(maxWidth: .infinity, minHeight: plain ? 40 : 52)
            .background(forward ? Theme.action : plain ? .clear : Theme.raised, in: .capsule)
            #else
            .frame(minHeight: 34)
            .background(forward ? Theme.action : plain ? .clear : Theme.raised, in: .rect(cornerRadius: 8))
            #endif
            .opacity(enabled ? 1 : 0.45)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(running)
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
