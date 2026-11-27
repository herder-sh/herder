import Foundation
import Herder
import Observation
import SwiftUI

/// The provider logins in a hub's CLIProxyAPI, for its owners: listed without their tokens,
/// added through the provider's sign-in, and removed. Each change goes to the hub, which drives
/// the proxy on its own machine.
@MainActor @Observable
final class HubLogins {
    typealias Send = @MainActor (CommandBody) async throws -> CommandResult

    let hub: HostId
    private let send: Send
    private(set) var list: Load<[ProviderLogin]> = .asking

    init(hub: HostId, send: @escaping Send) {
        self.hub = hub
        self.send = send
    }

    /// The logins of `hub`, sent through `fleet`.
    convenience init(fleet: Fleet, hub: HostId) {
        let client = fleet.client
        self.init(hub: hub) { try await client.send(hostId: hub, command: $0) }
    }

    var logins: [ProviderLogin] {
        if case .loaded(let logins) = list { logins } else { [] }
    }

    /// Asks the hub for its proxy's logins.
    func reload() async {
        do {
            guard case .providerLogins(let logins) = try await send(.listProviderLogins) else {
                throw HerderError.Local(detail: "the hub did not list its provider logins")
            }
            list = .loaded(logins)
        } catch {
            list = .failed(describe(error))
        }
    }

    /// Has the proxy start signing in to `provider`: the login and the URL to sign in at. With
    /// `again`, it signs that login in again, replacing its tokens in place: it keeps its name,
    /// pin, label and fallback mark, so it stays the same account on every host.
    func start(_ provider: Provider, again: String? = nil) async throws -> (loginId: String, url: String) {
        guard case .providerLoginStarted(let loginId, let url) = try await send(.startProviderLogin(provider: provider, name: again)) else {
            throw HerderError.Local(detail: "the hub did not start the login")
        }
        return (loginId, url)
    }

    /// Hands the proxy the address the browser was sent to; the list once it holds the login.
    func complete(_ loginId: String, redirectUrl: String) async throws {
        let address = redirectUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        guard case .providerLogins(let logins) = try await send(.completeProviderLogin(loginId: loginId, redirectUrl: address)) else {
            throw HerderError.Local(detail: "the hub did not finish the login")
        }
        list = .loaded(logins)
    }

    /// Stops a login that is waiting; one that is no longer waiting needs nothing.
    func cancel(_ loginId: String) async {
        _ = try? await send(.cancelProviderLogin(loginId: loginId))
    }

    /// Why the hub refused the last change to each login, by its name; cleared by the next.
    private(set) var errors: [String: String] = [:]
    /// The logins with a change on its way.
    private(set) var changing: Set<String> = []

    /// Names `login` for every host it is an account on; an empty name goes back to its email.
    /// Whether the hub took it.
    @discardableResult
    func rename(_ login: ProviderLogin, to label: String) async -> Bool {
        let label = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard label != Self.ownName(login) else { return true }
        return await change(login, label: label, fallback: nil)
    }

    /// Marks `login` fallback-only, or primary again, on every host.
    func setFallback(_ login: ProviderLogin, _ fallback: Bool) async {
        guard fallback != login.fallback else { return }
        await change(login, label: nil, fallback: fallback)
    }

    @discardableResult
    private func change(_ login: ProviderLogin, label: String?, fallback: Bool?) async -> Bool {
        errors[login.name] = nil
        changing.insert(login.name)
        defer { changing.remove(login.name) }
        do {
            guard case .providerLogins(let logins) = try await send(.setProviderLogin(name: login.name, label: label,
                                                                                         fallback: fallback)) else {
                throw HerderError.Local(detail: "the hub did not list its provider logins")
            }
            list = .loaded(logins)
            return true
        } catch {
            errors[login.name] = describe(error)
            return false
        }
    }

    func remove(_ login: ProviderLogin) async throws {
        _ = try await send(.removeProviderLogin(name: login.name))
        await reload()
    }

    /// What is left of `login`'s session and weekly windows, as it last reported them; nil for
    /// one it never reported.
    nonisolated static func windows(_ login: ProviderLogin, now: Date = .now) -> (session: WindowLeft?, weekly: WindowLeft?) {
        (ProviderAccounts.latest("Session", of: login.usage, now: now),
         ProviderAccounts.latest("Weekly", of: login.usage, now: now))
    }

    /// The name the owner gave `login`; empty while it goes by its email.
    nonisolated static func ownName(_ login: ProviderLogin) -> String {
        login.label == login.email ? "" : login.label ?? ""
    }

    /// The email under a login's label, when the label is the owner's own name for it.
    nonisolated static func email(_ login: ProviderLogin) -> String? {
        guard let email = login.email, email != login.label else { return nil }
        return email
    }

    /// The logins whose tokens the provider no longer takes: sessions on them fail until their
    /// owner logs them in again.
    var signedOut: [ProviderLogin] {
        logins.filter { $0.state == .needsSignIn }
    }

    /// The providers the proxy signs in to from an app.
    nonisolated static let providers: [Provider] = ["claude", "codex"]

    /// "In use", "Cooling down until 14:05", ...: whether the proxy can use `login` now.
    nonisolated static func stateLabel(_ login: ProviderLogin, now: Date = .now) -> String {
        switch login.state {
        case .active: return "In use"
        case .coolingDown:
            guard let until = login.until.flatMap(Timestamp.date), until > now else { return "Cooling down" }
            return "Cooling down, back in \(Timestamp.age(now, now: until))"
        case .needsSignIn: return "Needs signing in"
        case .error: return "Failing"
        case .disabled: return "Turned off"
        }
    }

    /// What the Logins tab says above its list while `signedOut` is not empty.
    nonisolated static func signedOutNotice(_ signedOut: [ProviderLogin]) -> String {
        let names = signedOut.map { $0.label ?? $0.name }
        guard names.count != 1 else {
            return "\(names[0]) needs signing in again: the provider no longer takes its tokens, so every session on it fails. Log in again below; it stays the same account on every host."
        }
        return "\(names.count) logins need signing in again: the provider no longer takes their tokens, so every session on them fails. Log each in again below; each stays the same account on every host."
    }

    nonisolated static func stateTint(_ state: ProviderLoginState) -> Color {
        switch state {
        case .active: Theme.success
        case .coolingDown: Theme.accent
        case .needsSignIn, .error: Theme.failure
        case .disabled: Theme.tertiary
        }
    }
}

/// Adding a login: pick a provider, sign in at the URL the proxy gives, then paste the address
/// the browser ended on. Or logging one in `again`, as its own provider and account, in place.
/// A login left before it is done is cancelled.
@MainActor @Observable
final class LoginAdder {
    let logins: HubLogins
    /// The login signed in again; nil when adding one.
    let again: ProviderLogin?
    var provider: Provider = "claude"
    private(set) var started: (loginId: String, url: String)?
    var address = ""
    private(set) var error: String?
    private(set) var sending = false
    private(set) var done = false

    init(logins: HubLogins, again: ProviderLogin? = nil) {
        self.logins = logins
        self.again = again
        if let again { provider = again.provider }
    }

    func start() async {
        await run { started = try await logins.start(provider, again: again?.name) }
    }

    /// Whether the proxy now holds the login.
    func complete() async -> Bool {
        guard let started else { return false }
        await run {
            try await logins.complete(started.loginId, redirectUrl: address)
            done = true
        }
        return done
    }

    /// Cancels the login if it started and did not finish.
    func leave() async {
        guard let started, !done else { return }
        self.started = nil
        await logins.cancel(started.loginId)
    }

    private func run(_ action: () async throws -> Void) async {
        error = nil
        sending = true
        defer { sending = false }
        do { try await action() } catch { self.error = describe(error) }
    }
}

/// The hub's provider logins, each with its state, what is left of its limits, an Add button,
/// and on each for owners Rename, a Fallback switch and Remove.
struct ProviderLoginsSection: View {
    let logins: HubLogins
    /// Whether this device's user owns the hub: only they rename a login or mark it fallback.
    var owner = true
    let add: () -> Void
    /// Logs a login in again, in place.
    var signInAgain: (ProviderLogin) -> Void = { _ in }
    let remove: (ProviderLogin) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 4) {
                SectionHeading(title: "Provider logins", count: logins.logins.count)
                InfoButton(text: "The logins in the CLIProxyAPI next to this hub. Each is an account on every host the hub serves, under the name you give it here; a fallback one is used only once the others are out. Their tokens stay on the hub; herder never sees them.")
                Spacer()
                PillButton(title: "Add login", symbol: "plus", prominent: true, action: add)
                    .accessibilityIdentifier("provider-login-add")
            }
            switch logins.list {
            case .asking:
                ProgressView().frame(maxWidth: .infinity)
            case .failed(let why):
                Text(why).font(.subheadline).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("provider-logins-error")
            case .loaded(let list) where list.isEmpty:
                Text("No logins yet. Add one, and hosts' endpoint accounts on this hub run on it.")
                    .font(.subheadline).foregroundStyle(Theme.tertiary)
            case .loaded(let list):
                if !logins.signedOut.isEmpty { signedOutNotice }
                VStack(spacing: 0) {
                    ForEach(Array(list.enumerated()), id: \.element.name) { index, login in
                        if index > 0 { Divider().overlay(Theme.stroke) }
                        ProviderLoginRow(login: login, logins: logins, owner: owner,
                                         signInAgain: { signInAgain(login) }, remove: { remove(login) })
                    }
                }
                .background(.card, in: .rect(cornerRadius: Theme.corner))
            }
        }
    }
}

extension ProviderLoginsSection {
    /// Above the list while any login is signed out: sessions on it fail until it is logged in
    /// again.
    private var signedOutNotice: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.failure)
            Text(HubLogins.signedOutNotice(logins.signedOut))
                .font(.subheadline).foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.failure.opacity(0.12), in: .rect(cornerRadius: Theme.corner))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("provider-logins-signed-out")
    }
}

/// A login: its label and email, provider and state, what is left of its session and weekly
/// windows, and for owners an inline rename, its Fallback switch and Remove. A login the
/// provider no longer takes says so under it, with Log in again. A change the hub refuses shows
/// under it.
private struct ProviderLoginRow: View {
    let login: ProviderLogin
    let logins: HubLogins
    let owner: Bool
    let signInAgain: () -> Void
    let remove: () -> Void
    @State private var renaming = false
    @State private var label = ""
    @FocusState private var focused: Bool

    var body: some View {
        let windows = HubLogins.windows(login)
        VStack(alignment: .leading, spacing: 8) {
            // On a phone the switch and buttons go on their own line, under the state.
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .center, spacing: 12) {
                    about
                    Spacer(minLength: 8)
                    state
                    if owner { controls }
                }
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        about
                        Spacer(minLength: 8)
                        state
                    }
                    if owner { controls }
                }
            }
            if windows.session != nil || windows.weekly != nil {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 16) {
                        limits(windows)
                        Spacer(minLength: 0)
                    }
                    VStack(alignment: .leading, spacing: 6) { limits(windows) }
                }
                .font(.caption)
            }
            if login.state == .needsSignIn { signedOut }
            if let error = logins.errors[login.name] {
                Text(error).font(.caption).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("provider-login-error-\(login.name)")
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 52)
        .contentShape(.rect)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("provider-login-\(login.name)")
        .onChange(of: focused) { if !focused && renaming { save() } }
    }

    /// Its label, or the field renaming it, and its provider, email and why it is not in use.
    private var about: some View {
        VStack(alignment: .leading, spacing: 3) {
            if renaming { field } else { title }
            HStack(spacing: 6) {
                Text(ModelCatalog.providerName(login.provider)).foregroundStyle(Theme.secondary)
                if let email = HubLogins.email(login) {
                    Text("· \(email)").foregroundStyle(Theme.tertiary).lineLimit(1).truncationMode(.middle)
                }
                if let message = login.message, login.state != .active {
                    Text("· \(message)").foregroundStyle(Theme.tertiary).lineLimit(1)
                }
            }
            .font(.caption)
        }
    }

    private var state: some View {
        Chip(text: HubLogins.stateLabel(login), tint: HubLogins.stateTint(login.state)).fixedSize()
    }

    /// Why sessions on it fail, and for owners Log in again.
    private var signedOut: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: 12) {
                signedOutText
                Spacer(minLength: 8)
                if owner { signInAgainButton }
            }
            VStack(alignment: .leading, spacing: 8) {
                signedOutText
                if owner { signInAgainButton }
            }
        }
        .padding(10)
        .background(Theme.failure.opacity(0.12), in: .rect(cornerRadius: 8))
    }

    private var signedOutText: some View {
        Text("Its tokens no longer work: sessions on it fail until you log in again, as \(login.email ?? "its own account").")
            .font(.caption).foregroundStyle(Theme.text)
            .fixedSize(horizontal: false, vertical: true)
    }

    private var signInAgainButton: some View {
        PillButton(title: "Log in again", symbol: "arrow.clockwise", prominent: true, action: signInAgain)
            .fixedSize()
            .accessibilityIdentifier("provider-login-again-\(login.name)")
    }

    @ViewBuilder private func limits(_ windows: (session: WindowLeft?, weekly: WindowLeft?)) -> some View {
        if let session = windows.session { limit("5-hour", session) }
        if let weekly = windows.weekly { limit("Weekly", weekly) }
    }

    private var title: some View {
        HStack(spacing: 6) {
            Text(login.label ?? login.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                .lineLimit(1).truncationMode(.middle)
            if login.fallback { Badge(text: "Fallback") }
        }
    }

    private var field: some View {
        TextField(login.email ?? login.name, text: $label)
            .textFieldStyle(.plain)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Theme.text)
            .autocorrectionDisabled()
            .focused($focused)
            .onSubmit { save() }
            .padding(.horizontal, 8)
            .frame(maxWidth: 280, minHeight: 28)
            .background(Theme.raised, in: .rect(cornerRadius: 6))
            .accessibilityIdentifier("provider-login-label-\(login.name)")
    }

    /// Rename, the Fallback switch and Remove.
    private var controls: some View {
        HStack(spacing: 8) {
            Toggle("Fallback", isOn: Binding(get: { login.fallback },
                                             set: { on in Task { await logins.setFallback(login, on) } }))
                .toggleStyle(.switch)
                .controlSize(.mini)
                .font(.caption)
                .foregroundStyle(Theme.secondary)
                .fixedSize()
                .disabled(logins.changing.contains(login.name))
                .help("A fallback login is used only once every other login of its provider is at its limit or signed out.")
                .accessibilityIdentifier("provider-login-fallback-\(login.name)")
            IconButton(symbol: renaming ? "checkmark" : "pencil", help: renaming ? "Save Name" : "Rename Login") {
                if renaming { save() } else { startRenaming() }
            }
            .accessibilityIdentifier("provider-login-rename-\(login.name)")
            IconButton(symbol: "trash", help: "Remove Login", action: remove)
                .accessibilityIdentifier("provider-login-remove-\(login.name)")
        }
    }

    private func limit(_ name: String, _ window: WindowLeft) -> some View {
        HStack(spacing: 6) {
            Text(name).foregroundStyle(Theme.secondary)
            UsageBar(percent: window.percentUsed).frame(width: ProvidersColumns.bar)
            Text("\(Int(window.percentLeft.rounded()))% left").monospacedDigit().foregroundStyle(Theme.text)
            if !window.resets.isEmpty {
                Text("· \(window.resets)").foregroundStyle(Theme.tertiary)
            }
        }
        .lineLimit(1)
        .fixedSize()
        .accessibilityElement(children: .combine)
    }

    private func startRenaming() {
        label = HubLogins.ownName(login)
        renaming = true
        focused = true
    }

    private func save() {
        guard renaming else { return }
        renaming = false
        let label = label
        Task { await logins.rename(login, to: label) }
    }
}

/// The sheet that adds a login, in two steps: pick a provider and start, then sign in and paste
/// the address the browser ended on.
struct AddProviderLoginSheet: View {
    @Bindable var adder: LoginAdder
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    var body: some View {
        SheetScaffold(title: adder.again == nil ? "Add Login" : "Log In Again",
                      subtitle: adder.again.map { "\($0.label ?? $0.name), in place: it stays the same account on every host" }
                          ?? "To the CLIProxyAPI next to this hub",
                      height: 480,
                      hint: adder.started == nil ? "Its tokens stay on the hub." : "The login waits five minutes.") {
            if adder.again == nil { providerPicker }
            if let started = adder.started {
                Field(label: "1. Sign in", hint: adder.again.map { "In your browser, as \($0.email ?? "the login's own account"); another account is refused." }
                          ?? "In your browser, as the account to add.") {
                    HStack(spacing: 10) {
                        Text(started.url).font(.caption.monospaced()).foregroundStyle(Theme.secondary)
                            .lineLimit(1).truncationMode(.middle)
                        Spacer(minLength: 0)
                        PillButton(title: "Copy", symbol: "doc.on.doc") { Clipboard.string = started.url }
                        PillButton(title: "Open sign-in page", symbol: "safari", prominent: true) {
                            if let url = URL(string: started.url) { openURL(url) }
                        }
                        .accessibilityIdentifier("provider-login-open")
                    }
                }
                Field(label: "2. Paste the address", hint: "The browser then goes to a localhost page that does not load. Copy its whole address from the address bar.") {
                    InputBox(placeholder: "http://localhost:…/callback?code=…", text: $adder.address, mono: true)
                        .accessibilityIdentifier("provider-login-address")
                }
            }
            if let error = adder.error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("provider-login-error")
            }
        } footer: {
            Spacer()
            if adder.started == nil {
                ActionButton(title: "Start", style: .primary) { await adder.start() }
                    .frame(maxWidth: 200)
                    .disabled(adder.sending)
                    .accessibilityIdentifier("provider-login-start")
            } else {
                ActionButton(title: adder.again == nil ? "Add Login" : "Log In Again", style: .primary) {
                    if await adder.complete() { dismiss() }
                }
                .frame(maxWidth: 200)
                .disabled(adder.sending || adder.address.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier("provider-login-complete")
            }
        }
        .onDisappear { Task { await adder.leave() } }
    }

    private var providerPicker: some View {
        Field(label: "Provider") {
            Picker("Provider", selection: $adder.provider) {
                ForEach(HubLogins.providers, id: \.self) { Text(ModelCatalog.providerName($0)).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .disabled(adder.started != nil)
            .accessibilityIdentifier("provider-login-provider")
        }
    }
}
