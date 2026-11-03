import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

private let now = Date(timeIntervalSince1970: 1_800_000_000)

private func stamp(_ date: Date) -> String { ISO8601DateFormatter().string(from: date) }

private func account(_ id: AccountId, configDir: String? = nil, signedOut: Date? = nil, used: [String: Double] = [:],
                     now: Date) -> Account {
    // The session window resets in 2 h 10 min, the weekly one in four days.
    let resets = ["five_hour": now.addingTimeInterval(7_800), "seven_day": now.addingTimeInterval(4 * 86_400 + 3_600)]
    return Account(accountId: id, provider: "claude", label: "work", configDir: configDir, email: "work@example.com",
                   usage: used.map { UsageWindow(window: $0.key, usedPercent: $0.value, resetsAt: resets[$0.key].map(stamp)) },
                   fallback: false, needsLogin: signedOut.map(stamp))
}

/// Three machines: signed in on work-mbp, signed out on trash-can-01 a quarter of an hour
/// before `now`, missing on draugelis.
private func machines(now: Date = now) -> [Machine] {
    [
        machine("a", name: "work-mbp", sessions: ["01A"],
                accounts: [account("main", used: ["five_hour": 82, "seven_day": 36], now: now)]),
        machine("b", name: "trash-can-01", sessions: [],
                accounts: [account("claude-work", configDir: "~/.herder/accounts/work",
                                   signedOut: now.addingTimeInterval(-900), now: now)]),
        machine("c", name: "draugelis", sessions: []),
    ]
}

private func endpointAccount(_ id: AccountId, on url: String, provider: Provider = "claude") -> Account {
    Account(accountId: id, provider: provider, label: "Proxy", configDir: "~/.herder/accounts/\(id)", email: nil,
            usage: [], endpoint: url)
}

struct AccountPageTests {
    @Test func endpointAccountsAreOneLoginPerEndpointAcrossMachines() throws {
        let fleet = [
            machine("a", name: "work-mbp", sessions: [],
                    accounts: [endpointAccount("claude-proxy", on: "http://127.0.0.1:8317"),
                               endpointAccount("claude-vault", on: "https://vault.lan:8317")]),
            machine("b", name: "trash-can-01", sessions: [],
                    accounts: [endpointAccount("claude", on: "http://127.0.0.1:8317")]),
        ]
        let logins = try #require(ProviderAccounts(machines: fleet, now: now).groups.first?.logins)
        #expect(logins.map(\.id).sorted() == ["claude/endpoint:http://127.0.0.1:8317",
                                                "claude/endpoint:https://vault.lan:8317"])
        let local = try #require(logins.first { $0.endpoint == "http://127.0.0.1:8317" })
        #expect(local.on.map(\.name) == ["work-mbp", "trash-can-01"])
        #expect(local.title == "Proxy" && local.email == nil)
        // Set up elsewhere, it keeps the endpoint, and needs the key typed again.
        let draft = try #require(logins.first { $0.endpoint == "https://vault.lan:8317" }).draft(taken: [])
        #expect(draft.usesEndpoint && draft.url == "https://vault.lan:8317" && draft.key.isEmpty)
        #expect(draft.configDir.isEmpty)
    }

    @Test func anEndpointAccountsEditKeepsItsDirectoryAndSendsOnlyAReplacedKey() {
        let account = endpointAccount("claude-proxy", on: "http://127.0.0.1:8317")
        // A label or rotation change sends no endpoint, key or config dir: absent keeps them.
        guard case .setAccountSettings(let id, let label, let dir, let fallback, let endpoint, let key, _)
            = account.settings(label: "Renamed", configDir: account.configDir, fallback: true) else {
            Issue.record("expected setAccountSettings")
            return
        }
        #expect(id == "claude-proxy" && label == "Renamed" && fallback == true)
        #expect(dir == nil && endpoint == nil && key == nil)
        guard case .setAccountSettings(_, _, let dir, _, let endpoint, let key, _)
            = account.settings(label: "Proxy", configDir: nil, fallback: nil, endpoint: "https://vault.lan", key: "sk-2") else {
            Issue.record("expected setAccountSettings")
            return
        }
        #expect(dir == nil && endpoint == "https://vault.lan" && key == "sk-2")
        // A login account never sends an endpoint or a key; its config dir goes as given.
        let login = Account(accountId: "main", provider: "claude", label: "work", configDir: "~/.claude-work",
                            email: "work@example.com", usage: [])
        guard case .setAccountSettings(_, _, let dir, _, let endpoint, let key, _)
            = login.settings(label: "work", configDir: "~/.claude-work", fallback: nil, endpoint: "http://x", key: "k") else {
            Issue.record("expected setAccountSettings")
            return
        }
        #expect(dir == "~/.claude-work" && endpoint == nil && key == nil)
    }

    @Test func eachMachineSaysHowTheLoginStandsThere() throws {
        let login = try #require(ProviderAccounts(machines: machines(), now: now).groups.first?.logins.first)
        var script = Script("01A", host: "a")
        let lists = Lists(machines: machines(), sessions: [script.key: script.model([created()])])
        let rows = AccountMachine.rows(login, machines: machines(), summaries: lists.machines)
        #expect(rows.map(\.place.name) == ["work-mbp", "trash-can-01", "draugelis"])
        #expect(rows.map(\.state) == [.signedIn, .signedOut(since: now.addingTimeInterval(-900)), .notSetUp])
        #expect(rows.map(\.sessions) == [1, 0, 0])
        #expect(rows.map { $0.account?.configDir } == [nil, "~/.herder/accounts/work", nil])
    }

    @Test func aResetSaysTheClockAndHowLongUntil() throws {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = try #require(TimeZone(identifier: "UTC"))
        calendar.locale = Locale(identifier: "en_GB")
        // 1_800_000_000 is Fri 15 Jan 2027, 08:00 UTC.
        #expect(WindowLeft.resetLine(now.addingTimeInterval(7_800), now: now, calendar: calendar)
                == "resets 10:10 · in 2 h 10 min")
        #expect(WindowLeft.resetLine(now.addingTimeInterval(2_400), now: now, calendar: calendar)
                == "resets 8:40 · in 40 min")
        #expect(WindowLeft.resetLine(now.addingTimeInterval(6 * 86_400 + 3_600), now: now, calendar: calendar)
                == "resets Thu 9:00 · in 6 d 1 h")
        #expect(WindowLeft.resetLine(nil, now: now, calendar: calendar) == "")
    }

    @Test func theWindowsKeepWhenTheyReset() throws {
        let login = try #require(ProviderAccounts(machines: machines(), now: now).groups.first?.logins.first)
        #expect(login.session?.resetsAt == now.addingTimeInterval(7_800))
        #expect(login.weekly?.percentLeft == 64)
    }
}

@MainActor struct AccountSignInTests {
    @Test func signingInAgainStartsTheLoginInActivity() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("no profile")
            return
        }
        fleet.setMachinesForTesting(machines())
        let login = try #require(ProviderAccounts(machines: fleet.machines).groups.first?.logins.first)
        let rows = AccountMachine.rows(login, machines: fleet.machines, summaries: fleet.lists.machines)
        let signedOut = try #require(rows.first { if case .signedOut = $0.state { true } else { false } })
        let key = try #require(signedOut.signIn(in: fleet))
        #expect(key == JobKey(hostId: "b", kind: .relogin("claude-work")))
        #expect(fleet.activity.keys.contains(key))
        // A machine without the login has nothing to sign in again.
        #expect(rows.last?.signIn(in: fleet) == nil)
    }
}

#if os(macOS)
import AppKit

/// Draws an account's page over a fleet of three machines, one signed out and one without the
/// login, and attaches it; with `HERDER_SCREENSHOT_DIR` set, also writes it there as
/// `account-page.png`.
@MainActor struct AccountPageScreenshotTests {
    @Test func drawsAnAccountPage() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("no profile")
            return
        }
        fleet.setMachinesForTesting(machines(now: .now))
        let loginId = try #require(ProviderAccounts(machines: fleet.machines).groups.first?.logins.first?.id)
        let page = VStack(spacing: 0) {
            PageTrail(crumbs: ["Providers", Page.account(loginId: loginId).title(in: fleet)]) { _ in }
            AccountPage(fleet: fleet, loginId: loginId)
        }
        .background(Theme.canvas)
        let host = NSHostingView(rootView: page.environment(\.colorScheme, .dark))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: 1040, height: 900)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        #expect(rep.pixelsWide >= 1040)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "account-page.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("account-page.png"))
        }
    }
}
#endif
