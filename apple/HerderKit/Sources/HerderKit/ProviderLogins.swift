import Foundation
import Herder
import Observation
import SwiftUI

/// The provider logins in a vault's CLIProxyAPI, for its owners: listed without their tokens,
/// added through the provider's sign-in, and removed. Each change goes to the vault, which drives
/// the proxy on its own machine.
@MainActor @Observable
final class VaultLogins {
    typealias Send = @MainActor (CommandBody) async throws -> CommandResult

    let vault: HostId
    private let send: Send
    private(set) var list: Load<[ProviderLogin]> = .asking

    init(vault: HostId, send: @escaping Send) {
        self.vault = vault
        self.send = send
    }

    /// The logins of `vault`, sent through `fleet`.
    convenience init(fleet: Fleet, vault: HostId) {
        let client = fleet.client
        self.init(vault: vault) { try await client.send(hostId: vault, command: $0) }
    }

    var logins: [ProviderLogin] {
        if case .loaded(let logins) = list { logins } else { [] }
    }

    /// Asks the vault for its proxy's logins.
    func reload() async {
        do {
            guard case .providerLogins(let logins) = try await send(.listProviderLogins) else {
                throw HerderError.Local(detail: "the vault did not list its provider logins")
            }
            list = .loaded(logins)
        } catch {
            list = .failed(describe(error))
        }
    }

    /// Has the proxy start signing in to `provider`: the login and the URL to sign in at.
    func start(_ provider: Provider) async throws -> (loginId: String, url: String) {
        guard case .providerLoginStarted(let loginId, let url) = try await send(.startProviderLogin(provider: provider)) else {
            throw HerderError.Local(detail: "the vault did not start the login")
        }
        return (loginId, url)
    }

    /// Hands the proxy the address the browser was sent to; the list once it holds the login.
    func complete(_ loginId: String, redirectUrl: String) async throws {
        let address = redirectUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        guard case .providerLogins(let logins) = try await send(.completeProviderLogin(loginId: loginId, redirectUrl: address)) else {
            throw HerderError.Local(detail: "the vault did not finish the login")
        }
        list = .loaded(logins)
    }

    /// Stops a login that is waiting; one that is no longer waiting needs nothing.
    func cancel(_ loginId: String) async {
        _ = try? await send(.cancelProviderLogin(loginId: loginId))
    }

    func remove(_ login: ProviderLogin) async throws {
        _ = try await send(.removeProviderLogin(name: login.name))
        await reload()
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
/// the browser ended on. A login left before it is done is cancelled.
@MainActor @Observable
final class LoginAdder {
    let logins: VaultLogins
    var provider: Provider = "claude"
    private(set) var started: (loginId: String, url: String)?
    var address = ""
    private(set) var error: String?
    private(set) var sending = false
    private(set) var done = false

    init(logins: VaultLogins) {
        self.logins = logins
    }

    func start() async {
        await run { started = try await logins.start(provider) }
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

/// The vault's provider logins, each with its state, an Add button and Remove on each.
struct ProviderLoginsSection: View {
    let logins: VaultLogins
    let add: () -> Void
    let remove: (ProviderLogin) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 4) {
                SectionHeading(title: "Provider logins", count: logins.logins.count)
                InfoButton(text: "The logins in the CLIProxyAPI next to this vault, which hosts reach through endpoint accounts. Their tokens stay on the vault; herder never sees them.")
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
                Text("No logins yet. Add one, and hosts' endpoint accounts on this vault run on it.")
                    .font(.subheadline).foregroundStyle(Theme.tertiary)
            case .loaded(let list):
                VStack(spacing: 0) {
                    ForEach(Array(list.enumerated()), id: \.element.name) { index, login in
                        if index > 0 { Divider().overlay(Theme.stroke) }
                        ProviderLoginRow(login: login) { remove(login) }
                    }
                }
                .background(.card, in: .rect(cornerRadius: Theme.corner))
            }
        }
    }
}

/// A login: its provider, account and state, and Remove.
private struct ProviderLoginRow: View {
    let login: ProviderLogin
    let remove: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text(login.label ?? login.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    .lineLimit(1).truncationMode(.middle)
                HStack(spacing: 6) {
                    Text(ModelCatalog.providerName(login.provider)).foregroundStyle(Theme.secondary)
                    if let message = login.message, login.state != .active {
                        Text("· \(message)").foregroundStyle(Theme.tertiary).lineLimit(1)
                    }
                }
                .font(.caption)
            }
            Spacer(minLength: 8)
            Chip(text: VaultLogins.stateLabel(login), tint: VaultLogins.stateTint(login.state))
            IconButton(symbol: "trash", help: "Remove Login", action: remove)
                .accessibilityIdentifier("provider-login-remove-\(login.name)")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 52)
        .contentShape(.rect)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("provider-login-\(login.name)")
    }
}

/// The sheet that adds a login, in two steps: pick a provider and start, then sign in and paste
/// the address the browser ended on.
struct AddProviderLoginSheet: View {
    @Bindable var adder: LoginAdder
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    var body: some View {
        SheetScaffold(title: "Add Login", subtitle: "To the CLIProxyAPI next to this vault", height: 480,
                      hint: adder.started == nil ? "Its tokens stay on the vault." : "The login waits five minutes.") {
            Field(label: "Provider") {
                Picker("Provider", selection: $adder.provider) {
                    ForEach(VaultLogins.providers, id: \.self) { Text(ModelCatalog.providerName($0)).tag($0) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .disabled(adder.started != nil)
                .accessibilityIdentifier("provider-login-provider")
            }
            if let started = adder.started {
                Field(label: "1. Sign in", hint: "In your browser, as the account to add.") {
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
                ActionButton(title: "Add Login", style: .primary) {
                    if await adder.complete() { dismiss() }
                }
                .frame(maxWidth: 200)
                .disabled(adder.sending || adder.address.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier("provider-login-complete")
            }
        }
        .onDisappear { Task { await adder.leave() } }
    }
}
