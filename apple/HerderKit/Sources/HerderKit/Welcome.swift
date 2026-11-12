import Herder
import Observation
import SwiftUI
#if os(iOS)
import UIKit
#endif

/// Welcome: the first launch of a device with no machines. It asks one question, new to herder
/// or already using it. Someone new goes through three steps, each with one button forward:
/// their name (the person's and the passkey's), installing herder on a machine, and approving
/// it with a passkey made then. Someone who already uses herder scans a code from another of
/// their devices, so they never become a second person with a second passkey.
@MainActor @Observable
final class WelcomeFlow {
    enum Step: Equatable {
        /// New to herder, or already using it.
        case choice
        /// Your name, for the person and the passkey's label.
        case name
        /// Installing herder on a machine, and taking the link it prints.
        case machine
        /// Making the machine yours with a new passkey.
        case approve
        /// The machine is yours: one next step.
        case done
    }

    /// Where the first machine's herder comes from, which decides what to run there.
    enum Install: CaseIterable {
        /// Installed now, on Linux or a Mac.
        case linuxOrMac
        /// herder's image, on Kubernetes or Docker.
        case container
        /// The machine runs herder already.
        case hasHerder
    }

    /// Makes the machine of `request` yours, as a new person called `name` with a passkey made
    /// now: its host id.
    typealias Approve = @MainActor (_ request: ApprovalRequest, _ name: String) async throws -> HostId?

    var step = Step.choice
    var name: String
    var install = Install.linuxOrMac
    /// The machine the scanned or pasted link offers.
    private(set) var request: ApprovalRequest?
    private(set) var approving = false
    /// What went wrong with the link or the approval, to show above the buttons.
    private(set) var problem: String?
    /// The machine, once yours.
    private(set) var machine: HostId?

    init(name: String = Fleet.deviceUserName) {
        self.name = name
    }

    /// The name as it will be saved.
    var person: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// Which of You · Machine · Approve the step is, and 3 once all are done; nil for the choice.
    var progress: Int? {
        switch step {
        case .choice: nil
        case .name: 0
        case .machine: 1
        case .approve: 2
        case .done: 3
        }
    }

    /// Whether a machine added another way, such as from another device's code, ends Welcome:
    /// not while the new path is approving its machine or showing it is done.
    var yieldsToAMachine: Bool { step != .approve && step != .done }

    func begin() { go(.name) }

    func confirmName() {
        guard !person.isEmpty else { return }
        go(.machine)
    }

    func back() {
        switch step {
        case .choice, .done: break
        case .name: go(.choice)
        case .machine: go(.name)
        case .approve: go(.machine)
        }
    }

    /// Takes the link in text pasted. See `take(_:)` of a link.
    @discardableResult
    func take(_ text: String?) -> FleetLink? {
        let text = text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard let link = pairLink(in: text) else {
            problem = text.isEmpty ? "Copy the herder://own link the machine prints, then paste it."
                : "That is not the herder://own link the machine prints."
            return nil
        }
        return take(link)
    }

    /// Takes a link scanned or pasted: a new machine's goes on to approve it; the machines
    /// another device of yours shares are returned, to sign in to as you instead.
    @discardableResult
    func take(_ link: PairLink) -> FleetLink? {
        switch link {
        case .approve(let request) where request.kind == .machine:
            self.request = request
            go(.approve)
        case .approve:
            problem = "That code signs a terminal in. The new machine prints its own code when herder is installed, or with `herder claim`."
        case .fleet(let shared):
            problem = nil
            return shared
        }
        return nil
    }

    /// Makes the machine yours with `approve`. Closing the passkey prompt leaves it to try again.
    func approve(with approve: Approve) async {
        guard let request, !approving else { return }
        approving = true
        problem = nil
        defer { approving = false }
        do {
            machine = try await approve(request, person)
            step = .done
        } catch where isCancelled(error) {
            return
        } catch {
            problem = describe(error)
        }
    }

    private func go(_ step: Step) {
        problem = nil
        self.step = step
    }
}

extension WelcomeFlow.Install {
    var title: String {
        switch self {
        case .linuxOrMac: "Linux or Mac"
        case .container: "Kubernetes or Docker"
        case .hasHerder: "It already runs herder"
        }
    }

    var detail: String {
        switch self {
        case .linuxOrMac: "Install herder and run it as a service"
        case .container: "Run herder’s image, then ask it for a code"
        case .hasHerder: "Run herder claim there for a code"
        }
    }

    var symbol: String {
        switch self {
        case .linuxOrMac: "desktopcomputer"
        case .container: "shippingbox"
        case .hasHerder: "terminal"
        }
    }

    /// What to run, and where.
    var lead: String {
        switch self {
        case .linuxOrMac: "The machine runs your agents: a server, a desktop, or a Mac. Run this there:"
        case .container: "Where herder’s container runs, ask it for a code:"
        case .hasHerder: "On the machine, run:"
        }
    }

    var command: String {
        switch self {
        case .linuxOrMac: "curl -fsSL https://herder.sh/install | sh\nherder service install"
        case .container: "kubectl exec -it <pod> -- /herder claim"
        case .hasHerder: PairKind.command
        }
    }
}

extension EnvironmentValues {
    /// Draws Welcome as on a phone: a back chevron at the top, buttons docked at the bottom. A
    /// test sets it to draw the phone's layout on the Mac.
    @Entry var welcomeCompact: Bool = {
        #if os(iOS)
        true
        #else
        false
        #endif
    }()
    /// The device Welcome is on, as it names it ("iPhone"); nil for this one.
    @Entry var welcomeDevice: String?
}

/// Welcome, on a fleet with no machines: the flow's step, full screen, in place of the fleet.
/// `existing` opens the scan from another device; `join` signs in to the machines of a fleet
/// link scanned instead of a machine's; `invite`, where there is one, opens a teammate's
/// invite; `finish` ends Welcome, opening what the done step chose.
struct WelcomeView: View {
    let flow: WelcomeFlow
    let fleet: Fleet
    let existing: () -> Void
    let join: (FleetLink) -> Void
    var invite: (() -> Void)?
    let finish: (AppSheet?) -> Void
    @State private var scanning = false

    var body: some View {
        Group {
            #if os(iOS)
            if scanning {
                PairScanner(scanned: { scanned($0) }, close: { scanning = false })
            } else {
                screen
            }
            #else
            screen
            #endif
        }
        .onAppear { Launch.mark(.content) }
    }

    private var screen: some View {
        WelcomeScreen(flow: flow, invite: invite, existing: existing, scan: scan, paste: { paste() },
                      approve: { await flow.approve { request, name in
                          try await fleet.approve(request, with: passkeyAuthenticator(), newPasskey: name)
                      } },
                      finish: finish)
    }

    private var scan: (() -> Void)? {
        #if os(iOS)
        { scanning = true }
        #else
        nil
        #endif
    }

    private func paste() {
        if let link = flow.take(Clipboard.string) { join(link) }
    }

    private func scanned(_ link: PairLink) {
        if let shared = flow.take(link) { join(shared) }
    }
}

/// Welcome's step as the flow stands, drawn from the flow and what its buttons do alone, so a
/// test can draw each step without a fleet.
struct WelcomeScreen: View {
    let flow: WelcomeFlow
    var invite: (() -> Void)?
    let existing: () -> Void
    /// Opens the camera, where there is one.
    var scan: (() -> Void)?
    let paste: () -> Void
    let approve: () async -> Void
    let finish: (AppSheet?) -> Void
    var biometry = Biometry.name
    @Environment(\.welcomeCompact) private var compact
    @Environment(\.welcomeDevice) private var device
    @Environment(\.exploreDemo) private var explore
    @FocusState private var naming: Bool

    /// Where a passkey made now is kept: the platform's own store, unless a password manager
    /// is set to keep passkeys, which the system asks about as it saves it.
    static let passkeyHome = "Apple Passwords or your password manager"

    var body: some View {
        switch flow.step {
        case .choice: choice
        case .name: nameStep
        case .machine: machineStep
        case .approve: approveStep
        case .done: doneStep
        }
    }

    /// "iPhone": this device, as Welcome names it.
    private var here: String { device ?? Self.thisDevice }
    /// The device someone who already uses herder most likely has it on.
    private var other: String { here == "Mac" ? "iPhone" : "Mac" }

    @MainActor static var thisDevice: String {
        #if os(iOS)
        UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone"
        #else
        "Mac"
        #endif
    }

    // MARK: Welcome

    private var choice: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 28) {
                    VStack(spacing: 10) {
                        Wordmark()
                            .aspectRatio(Wordmark.size.width / Wordmark.size.height, contentMode: .fit)
                            .frame(maxWidth: compact ? 170 : 200)
                            .padding(.bottom, 18)
                            .accessibilityHidden(true)
                        Text(compact ? "Run your coding agents on your own machines" : "Welcome to herder")
                            .font(Font.welcomeTitle(compact: compact, mac: 30))
                            .foregroundStyle(Theme.text)
                        Text(compact ? "Start, watch and answer them from anywhere."
                             : "Run your coding agents on your own machines, and drive them from here.")
                            .font(compact ? Font.subheadline : Font.body)
                            .foregroundStyle(Theme.tertiary)
                    }
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, compact ? 96 : 72)
                    if !compact { choices }
                }
                .frame(maxWidth: welcomeColumn)
                .padding(.horizontal, compact ? 20 : 32)
                .padding(.bottom, 24)
                .frame(maxWidth: .infinity)
            }
            if compact {
                VStack(spacing: 10) { choices }
                    .frame(maxWidth: welcomeColumn)
                    .padding(.horizontal, 20)
                    .padding(.bottom, 12)
                    .frame(maxWidth: .infinity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .layer(.canvas)
    }

    @ViewBuilder private var choices: some View {
        VStack(spacing: 10) {
            WelcomeChoice(symbol: "plus", title: "I’m new to herder",
                          detail: "Set up your first machine. About 3 minutes.", highlighted: true) { flow.begin() }
                .accessibilityIdentifier("welcome-new")
            WelcomeChoice(symbol: "qrcode", title: "I already use herder",
                          detail: here == "Mac" ? "Add this Mac from your iPhone or another computer."
                              : "Add this \(here) from your Mac or another device.") { existing() }
                .accessibilityIdentifier("welcome-existing")
            if let invite {
                Button(action: invite) {
                    Text("Got an invite from a teammate? \(Text("Open it").foregroundStyle(Theme.link))")
                        .font(.footnote).foregroundStyle(Theme.tertiary)
                        .frame(minHeight: 32)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("welcome-invite")
            }
            if let explore {
                Button("Explore a demo", action: explore)
                    .buttonStyle(.plain)
                    .font(.footnote)
                    .foregroundStyle(Theme.link)
                    .frame(minHeight: 32)
                    .accessibilityIdentifier("explore-demo")
            }
        }
    }

    // MARK: 1 of 3 · You

    private var nameStep: some View {
        WelcomePage(progress: 0, title: "What should we call you?",
                    subtitle: "Your machines and teammates will know you by this name.", back: { flow.back() }) {
            TextField("Your name", text: Bindable(flow).name)
                .textFieldStyle(.plain)
                .font(.title3)
                .foregroundStyle(Theme.text)
                #if os(iOS)
                .textContentType(.name)
                #endif
                .focused($naming)
                .onSubmit { flow.confirmName() }
                .padding(.horizontal, 14)
                .frame(minHeight: 50)
                .background(.card, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
                .accessibilityIdentifier("welcome-name")
                .onAppear { naming = true }
            WelcomeNote(symbol: "key", text: Text("Your passkey is saved under this name. herder makes it when you approve your first machine, and uses the same one everywhere after."))
            Button(action: existing) {
                WelcomeNote(symbol: "person", text: Text("Already have a herder passkey? \(Text("Use it instead").foregroundStyle(Theme.link)), so you stay one person."))
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("welcome-use-passkey")
        } dock: {
            WelcomeButton(title: "Continue") { flow.confirmName() }
                .disabled(flow.person.isEmpty)
                .accessibilityIdentifier("welcome-continue")
        }
    }

    // MARK: 2 of 3 · Machine

    private var machineStep: some View {
        WelcomePage(progress: 1, title: "Install herder on a machine", subtitle: flow.install.lead, back: { flow.back() }) {
            WelcomeCommand(command: flow.install.command)
            WelcomeNote(symbol: "qrcode", text: scan == nil
                ? Text("It prints a herder://own link and a QR code. Copy the link, then paste it here.")
                : Text("It prints a QR code. Keep it on screen, then scan it."))
            SettingsList(title: "Where herder runs") {
                ForEach(Array(WelcomeFlow.Install.allCases.enumerated()), id: \.element) { index, install in
                    if index > 0 { RowDivider() }
                    SummaryRow(title: install.title, summary: install.detail, open: { flow.install = install }) {
                        Image(systemName: install.symbol).foregroundStyle(Theme.secondary).frame(width: 22)
                    } accessory: {
                        Image(systemName: "checkmark").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.action)
                            .opacity(install == flow.install ? 1 : 0)
                    }
                    .accessibilityAddTraits(install == flow.install ? .isSelected : [])
                }
            }
            WelcomeProblem(text: flow.problem)
        } dock: {
            if let scan {
                WelcomeButton(title: "Scan the Code", symbol: "camera", action: scan)
                    .accessibilityIdentifier("welcome-scan")
                WelcomeButton(title: "Paste a Link Instead", kind: .plain, action: paste)
                    .accessibilityIdentifier("welcome-paste")
            } else {
                WelcomeButton(title: "Paste the Link", symbol: "doc.on.clipboard", action: paste)
                    .accessibilityIdentifier("welcome-paste")
            }
        }
    }

    // MARK: 3 of 3 · Approve

    private var approveStep: some View {
        WelcomePage(progress: 2, title: "Make \(flow.request?.name ?? "it") yours",
                    hint: "Uses \(biometry) once.", back: { flow.back() }) {
            if let request = flow.request {
                WelcomeMachineCard(request: request)
            }
            VStack(alignment: .leading, spacing: 8) {
                SectionHeading(title: "You’ll sign in as")
                WelcomePasskeyCard(name: flow.person, detail: "New passkey · saved in \(Self.passkeyHome)")
            }
            .padding(.top, 6)
            WelcomeNote(symbol: "checkmark.shield", text: Text("Owners can do everything there, terminals included. You can add teammates later."))
            WelcomeProblem(text: flow.problem)
        } dock: {
            WelcomeButton(title: "Create Passkey and Approve", symbol: biometry == "Touch ID" ? "touchid" : "faceid",
                          busy: flow.approving) {
                Task { await approve() }
            }
            .disabled(flow.approving)
            .accessibilityIdentifier("welcome-approve")
        }
    }

    // MARK: Done

    private var doneStep: some View {
        let name = flow.request?.name ?? "Your machine"
        return WelcomePage(progress: 3) {
            VStack(spacing: 8) {
                Image(systemName: "checkmark")
                    .font(.system(size: 34, weight: .bold))
                    .foregroundStyle(.white)
                    .frame(width: 76, height: 76)
                    .background(Theme.success, in: .circle)
                    .padding(.bottom, 10)
                Text("\(name) is yours")
                    .font(Font.welcomeTitle(compact: compact, mac: 26))
                    .foregroundStyle(Theme.text)
                    .accessibilityIdentifier("welcome-done")
                Text("Your passkey “\(flow.person)” is saved in \(Self.passkeyHome).")
                    .font(.subheadline).foregroundStyle(Theme.tertiary)
            }
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
            SettingsList(title: "When you’re ready") {
                next("Add another machine", "Same three steps", symbol: "server.rack", .pair(.machine))
                RowDivider()
                next("Use herder on your \(other)", "Show a code it scans", symbol: other == "Mac" ? "laptopcomputer" : "iphone",
                     .addDevice)
                if let machine = flow.machine {
                    RowDivider()
                    next("Invite a teammate", "A code that lets them in", symbol: "person.2", .invite(machine))
                }
            }
        } dock: {
            WelcomeButton(title: "Start a Session") { finish(.newSession) }
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("welcome-start")
        }
    }

    private func next(_ title: String, _ summary: String, symbol: String, _ sheet: AppSheet) -> some View {
        SummaryRow(title: title, summary: summary, open: { finish(sheet) }) {
            Image(systemName: symbol).foregroundStyle(Theme.secondary).frame(width: 22)
        } accessory: {
            EmptyView()
        }
    }
}

extension Font {
    /// A Welcome step's title: the phone's title 2, larger on the Mac.
    static func welcomeTitle(compact: Bool, mac size: CGFloat) -> Font {
        compact ? .title2.weight(.bold) : .system(size: size, weight: .bold)
    }
}

/// The column Welcome reads in, on wide screens.
private let welcomeColumn: CGFloat = 520

/// A Welcome step's frame: the progress over its title, the scrolling content, and the dock with
/// its one forward button, at the bottom of a phone's screen or on the right on the Mac, where
/// Back sits on the left. On a phone Back is a chevron at the top.
struct WelcomePage<Content: View, Dock: View>: View {
    /// Which of You · Machine · Approve is on; 3 once all are done.
    var progress: Int?
    var title = ""
    var subtitle = ""
    /// One line about the forward button: under it on a phone, beside it on the Mac.
    var hint = ""
    var back: (() -> Void)?
    @ViewBuilder var content: Content
    @ViewBuilder var dock: Dock
    @Environment(\.welcomeCompact) private var compact

    var body: some View {
        VStack(spacing: 0) {
            if compact, let back {
                HStack {
                    Button(action: back) {
                        Image(systemName: "chevron.left")
                            .font(.body.weight(.semibold))
                            .foregroundStyle(Theme.text)
                            .frame(width: 40, height: 40)
                            .background(Theme.raised, in: .circle)
                            .contentShape(.circle)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Back")
                    .accessibilityIdentifier("welcome-back")
                    Spacer()
                }
                .padding(.horizontal, 16)
                .padding(.top, 4)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let progress { WelcomeProgress(step: progress).padding(.bottom, 10) }
                    if !title.isEmpty {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(title)
                                .font(Font.welcomeTitle(compact: compact, mac: 26))
                                .foregroundStyle(Theme.text)
                            if !subtitle.isEmpty {
                                Text(subtitle).font(compact ? Font.subheadline : Font.body).foregroundStyle(Theme.tertiary)
                            }
                        }
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.bottom, 6)
                    }
                    content
                }
                .frame(maxWidth: welcomeColumn, alignment: .leading)
                .padding(.horizontal, compact ? 20 : 32)
                .padding(.top, compact ? 12 : 56)
                .padding(.bottom, 24)
                .frame(maxWidth: .infinity)
            }
            if compact {
                VStack(spacing: 10) {
                    dock
                    if !hint.isEmpty {
                        Text(hint).font(.footnote).foregroundStyle(Theme.tertiary).multilineTextAlignment(.center)
                    }
                }
                .frame(maxWidth: welcomeColumn)
                .padding(.horizontal, 20)
                .padding(.top, 8)
                .padding(.bottom, 12)
                .frame(maxWidth: .infinity)
            } else {
                HStack(spacing: 12) {
                    if let back {
                        WelcomeButton(title: "Back", kind: .secondary, action: back)
                            .keyboardShortcut(.cancelAction)
                            .accessibilityIdentifier("welcome-back")
                    }
                    Spacer()
                    if !hint.isEmpty {
                        Text(hint).font(.footnote).foregroundStyle(Theme.tertiary)
                    }
                    dock
                }
                .frame(maxWidth: welcomeColumn)
                .padding(.horizontal, 32)
                .padding(.vertical, 24)
                .frame(maxWidth: .infinity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .layer(.canvas)
    }
}

/// The new path's progress: three bars, You · Machine · Approve, filled up to the step on.
struct WelcomeProgress: View {
    /// 0, 1 or 2 for the step on; 3 once all are done.
    let step: Int

    static let steps = ["You", "Machine", "Approve"]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                ForEach(Self.steps.indices, id: \.self) { index in
                    Capsule().fill(index <= step ? Theme.action : Theme.raised).frame(height: 4)
                }
            }
            if step < Self.steps.count {
                HStack(spacing: 6) {
                    ForEach(Self.steps.indices, id: \.self) { index in
                        Text(Self.steps[index])
                            .font(.caption.weight(index == step ? .semibold : .regular))
                            .foregroundStyle(index == step ? Theme.text : Theme.tertiary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(step < Self.steps.count ? "Step \(step + 1) of 3, \(Self.steps[step])" : "All 3 steps done")
    }
}

/// A Welcome button: the one forward step in the action colour, a capsule docked on a phone and
/// a filled button on the Mac; a quiet secondary one (Back); or plain text in the link colour.
struct WelcomeButton: View {
    enum Kind { case forward, secondary, plain }
    let title: String
    var symbol: String?
    var kind = Kind.forward
    var busy = false
    let action: () -> Void
    @Environment(\.welcomeCompact) private var compact
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(action: action) {
            HStack(spacing: 7) {
                if busy {
                    ProgressView().controlSize(.small).tint(.white)
                } else if let symbol {
                    Image(systemName: symbol)
                }
                Text(title)
            }
            .font(compact ? Font.body.weight(.semibold) : Theme.body.weight(.semibold))
            .foregroundStyle(foreground)
            .padding(.horizontal, compact ? 20 : 16)
            .frame(maxWidth: compact ? CGFloat.infinity : nil)
            .frame(height: compact ? (kind == .plain ? 40 : 52) : 34)
            .background(background, in: compact ? AnyShape(Capsule()) : AnyShape(RoundedRectangle(cornerRadius: 8)))
            .opacity(enabled ? 1 : 0.45)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }

    private var foreground: Color {
        switch kind {
        case .forward: .white
        case .secondary: Theme.text
        case .plain: Theme.link
        }
    }

    private var background: Color {
        switch kind {
        case .forward: Theme.action
        case .secondary: Theme.raised
        case .plain: .clear
        }
    }
}

/// One of Welcome's two doors: its mark, what it is and what follows, and a chevron.
struct WelcomeChoice: View {
    let symbol: String
    let title: String
    let detail: String
    var highlighted = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: symbol)
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(highlighted ? .white : Theme.text)
                    .frame(width: 44, height: 44)
                    .background(highlighted ? Theme.action : Theme.raised, in: .rect(cornerRadius: 12))
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.body.weight(.semibold)).foregroundStyle(Theme.text)
                    Text(detail).font(.footnote).foregroundStyle(Theme.tertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .multilineTextAlignment(.leading)
                Spacer(minLength: 8)
                Image(systemName: "chevron.right").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.card, in: .rect(cornerRadius: 16))
            .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(highlighted ? Theme.action.opacity(0.6) : Theme.stroke))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

/// A line of help with its mark, in secondary text.
struct WelcomeNote: View {
    let symbol: String
    let text: Text

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: symbol).foregroundStyle(Theme.tertiary).frame(width: 20)
            text.foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .font(.footnote)
        .multilineTextAlignment(.leading)
    }
}

/// What went wrong, where there is something.
struct WelcomeProblem: View {
    let text: String?

    var body: some View {
        if let text {
            Text(LocalizedStringKey(text)).font(.footnote).foregroundStyle(Theme.danger)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("welcome-problem")
        }
    }
}

/// The commands to run on the machine, ready to copy.
struct WelcomeCommand: View {
    let command: String

    var body: some View {
        HStack(alignment: .top) {
            Text(command)
                .font(Theme.mono)
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.vertical, 12)
            Spacer(minLength: 8)
            CopyButton(text: command).padding(.top, 6)
        }
        .padding(.leading, 14)
        .padding(.trailing, 6)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// The machine a link offers: its name, its address, and the code its terminal shows, small,
/// to compare.
struct WelcomeMachineCard: View {
    let request: ApprovalRequest

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "server.rack")
                .foregroundStyle(Theme.secondary)
                .frame(width: 36, height: 36)
                .background(Theme.raised, in: .rect(cornerRadius: 9))
            VStack(alignment: .leading, spacing: 2) {
                Text(request.name).font(.body.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
                if !request.about.isEmpty && request.about != request.name {
                    Text(request.about).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 2) {
                Text("Terminal shows").font(.caption).foregroundStyle(Theme.tertiary)
                Text(request.spokenCode)
                    .font(.system(.subheadline, design: .monospaced).weight(.semibold))
                    .foregroundStyle(Theme.text)
                    .accessibilityIdentifier("approval-code")
            }
        }
        .padding(12)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// The passkey an approval makes or uses: whose it is, and where it is kept.
struct WelcomePasskeyCard: View {
    let name: String
    let detail: String

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "key.fill")
                .foregroundStyle(Theme.action)
                .frame(width: 36, height: 36)
                .background(Theme.action.opacity(0.16), in: .rect(cornerRadius: 9))
            VStack(alignment: .leading, spacing: 2) {
                Text(name).font(.body.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
                Text(detail).font(.footnote).foregroundStyle(Theme.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .accessibilityElement(children: .combine)
    }
}
