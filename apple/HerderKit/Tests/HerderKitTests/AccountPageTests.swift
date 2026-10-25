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

struct AccountPageTests {
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
        .background(Theme.background)
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
