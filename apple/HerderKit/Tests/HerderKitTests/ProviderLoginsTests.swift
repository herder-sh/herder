import Foundation
import Herder
@testable import HerderKit
import Testing

/// A vault's CLIProxyAPI, as the app reaches it: it records what it is sent, starts logins,
/// and signs one in when the pasted address carries the code `good`.
@MainActor final class FakeLoginsVault {
    var sent: [CommandBody] = []
    var logins: [ProviderLogin] = sampleProviderLogins()

    func model() -> VaultLogins {
        VaultLogins(vault: "h-vault") { [self] command in
            sent.append(command)
            switch command {
            case .listProviderLogins:
                return .providerLogins(logins: logins)
            case .startProviderLogin(let provider):
                return .providerLoginStarted(loginId: "state1", url: "https://\(provider).example/oauth?state=state1")
            case .completeProviderLogin(_, let address):
                guard address.contains("code=good") else {
                    throw HerderError.Rejected(info: ErrorInfo(code: .badRequest, message: "that address has no code"))
                }
                logins.append(ProviderLogin(name: "claude-grace@example.com.json", provider: "claude",
                                            label: "grace@example.com", state: .active, until: nil, message: nil))
                return .providerLogins(logins: logins)
            case .removeProviderLogin(let name):
                logins.removeAll { $0.name == name }
                return .applied
            default:
                return .applied
            }
        }
    }
}

/// Three logins: one in use, one cooling down for an hour, one signed out.
func sampleProviderLogins(now: Date = .now) -> [ProviderLogin] {
    [
        ProviderLogin(name: "claude-ada@example.com.json", provider: "claude", label: "ada@example.com",
                      state: .active, until: nil, message: nil),
        ProviderLogin(name: "codex-ada@example.com-plus.json", provider: "codex", label: "ada@example.com",
                      state: .coolingDown, until: now.addingTimeInterval(3600).ISO8601Format(), message: "quota exhausted"),
        ProviderLogin(name: "claude-old.json", provider: "claude", label: nil,
                      state: .needsSignIn, until: nil, message: "unauthorized (refresh token invalid)"),
    ]
}

@MainActor struct ProviderLoginsTests {
    @Test func addingALoginStartsItThenHandsTheVaultThePastedAddress() async {
        let vault = FakeLoginsVault()
        let logins = vault.model()
        await logins.reload()
        #expect(logins.logins.count == 3)

        let adder = LoginAdder(logins: logins)
        await adder.start()
        #expect(adder.started?.url == "https://claude.example/oauth?state=state1")
        adder.address = "  http://localhost:54545/callback?state=state1  "
        #expect(await adder.complete() == false)
        #expect(adder.error == "that address has no code")
        adder.address = "http://localhost:54545/callback?code=good&state=state1\n"
        #expect(await adder.complete())
        #expect(logins.logins.map(\.label).contains("grace@example.com"))
        // Done: leaving cancels nothing.
        await adder.leave()
        #expect(vault.sent == [
            .listProviderLogins,
            .startProviderLogin(provider: "claude"),
            .completeProviderLogin(loginId: "state1", redirectUrl: "http://localhost:54545/callback?state=state1"),
            .completeProviderLogin(loginId: "state1", redirectUrl: "http://localhost:54545/callback?code=good&state=state1"),
        ])
    }

    @Test func leavingALoginBeforeItIsDoneCancelsIt() async {
        let vault = FakeLoginsVault()
        let adder = LoginAdder(logins: vault.model())
        adder.provider = "codex"
        // Not started: nothing to cancel.
        await adder.leave()
        await adder.start()
        await adder.leave()
        await adder.leave()
        #expect(vault.sent == [.startProviderLogin(provider: "codex"), .cancelProviderLogin(loginId: "state1")])
    }

    @Test func removingALoginListsAgain() async throws {
        let vault = FakeLoginsVault()
        let logins = vault.model()
        await logins.reload()
        try await logins.remove(logins.logins[2])
        #expect(vault.sent.last == .listProviderLogins)
        #expect(logins.logins.map(\.name) == ["claude-ada@example.com.json", "codex-ada@example.com-plus.json"])
    }

    @Test func aVaultWithNoProxySaysSo() async {
        let logins = VaultLogins(vault: "h-vault") { _ in
            throw HerderError.Rejected(info: ErrorInfo(code: .unsupported, message: "this vault runs no CLIProxyAPI"))
        }
        await logins.reload()
        guard case .failed(let why) = logins.list else {
            Issue.record("expected a refusal")
            return
        }
        #expect(why.contains("runs no CLIProxyAPI"))
    }

    @Test func eachStateReadsAsWhatItMeans() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let logins = sampleProviderLogins(now: now)
        #expect(VaultLogins.stateLabel(logins[0], now: now) == "In use")
        #expect(VaultLogins.stateLabel(logins[1], now: now).hasPrefix("Cooling down, back in 1"))
        #expect(VaultLogins.stateLabel(logins[1], now: now.addingTimeInterval(7200)) == "Cooling down")
        #expect(VaultLogins.stateLabel(logins[2], now: now) == "Needs signing in")
    }
}

#if os(macOS)
import AppKit
import SwiftUI

/// Draws the vault page's Logins tab, and the Add Login sheet at each step, as the app's dark
/// appearance shows them, and attaches them; with `HERDER_SCREENSHOT_DIR` set, also writes them
/// there.
@MainActor struct ProviderLoginsScreenshotTests {
    @Test func drawsTheLoginsTab() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.setMachinesForTesting(sampleFleetMachines())
        let vault = FakeLoginsVault()
        let page = VaultPage(fleet: fleet, vault: "h-vault", tab: .logins,
                             secrets: VaultSecrets(vault: "h-vault") { _ in .applied }, logins: vault.model())
        try await shoot(page, named: "vault-logins.png", width: 1100, height: 460)
    }

    @Test func drawsTheAddLoginSheetAtEachStep() async throws {
        let vault = FakeLoginsVault()
        try await shoot(AddProviderLoginSheet(adder: LoginAdder(logins: vault.model())), named: "add-login-start.png",
                        width: 560, height: 480)
        // Its own: closing a sheet cancels the login it started.
        let adder = LoginAdder(logins: vault.model())
        await adder.start()
        adder.address = "http://localhost:54545/callback?code=abc"
        try await shoot(AddProviderLoginSheet(adder: adder), named: "add-login-paste.png", width: 560, height: 480)
    }

    private func shoot(_ view: some View, named name: String, width: CGFloat, height: CGFloat) async throws {
        let host = NSHostingView(rootView: view.environment(\.colorScheme, .dark).background(Theme.canvas))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: width, height: height)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}
#endif
