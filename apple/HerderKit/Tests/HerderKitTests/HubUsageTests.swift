import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// What the sample hub's logins used over the week: Work on both machines, with two models and
/// three limit hits; the Codex login on one machine, which sessions failed over onto; and a
/// login the hub no longer holds.
func sampleHubUsage() -> (totals: [HubUsageTotal], failovers: [HubFailoverTotal]) {
    func total(_ pin: String, _ host: HostId, _ day: String, _ model: String, provider: Provider = "claude",
               turns: UInt64, tokens: UInt64, cost: Double, estimated: Bool = false) -> HubUsageTotal {
        HubUsageTotal(pin: pin, hostId: host, day: day,
                      usage: UsageTotal(accountId: "vault-\(pin)", provider: provider, model: model, turns: turns,
                                        input: tokens / 10, output: tokens / 10, cacheRead: tokens * 7 / 10,
                                        cacheWrite: tokens / 10, costUsd: cost, costEstimated: estimated))
    }
    let totals = [
        total("work", "work-mbp", "2026-11-25", "claude-opus-5-5", turns: 42, tokens: 9_400_000, cost: 61.20),
        total("work", "work-mbp", "2026-11-26", "claude-opus-5-5", turns: 18, tokens: 3_100_000, cost: 22.75),
        total("work", "trash-can-01", "2026-11-26", "claude-sonnet-5-5", turns: 30, tokens: 2_200_000, cost: 6.10),
        total("codex-ada", "trash-can-01", "2026-11-26", "gpt-5.5", provider: "codex", turns: 9, tokens: 820_000,
              cost: 3.40, estimated: true),
        total("gone", "work-mbp", "2026-11-21", "claude-opus-5-5", turns: 2, tokens: 40_000, cost: 0.30),
    ]
    let failovers = [
        HubFailoverTotal(pin: "work", hostId: "work-mbp",
                         failovers: FailoverTotal(accountId: "vault-work", limitHits: 3, failoversOut: 2, failoversIn: 0)),
        HubFailoverTotal(pin: "codex-ada", hostId: "trash-can-01",
                         failovers: FailoverTotal(accountId: "vault-codex-ada", limitHits: 0, failoversOut: 0,
                                                  failoversIn: 2)),
    ]
    return (totals, failovers)
}

/// The hub's answer to `get_hub_usage`, recording the periods asked.
@MainActor final class FakeUsageHub {
    var asked: [UsagePeriod] = []

    func model() -> HubUsage {
        HubUsage(hub: "h-hub") { [self] command in
            guard case .getHubUsage(let period) = command else { return .applied }
            asked.append(period)
            let usage = sampleHubUsage()
            return .hubUsage(period: period, since: "2026-11-20T12:00:00Z", totals: usage.totals,
                             failovers: usage.failovers)
        }
    }
}

@MainActor struct HubUsageTests {
    @Test func eachLoginAddsUpOverItsMachinesModelsAndDays() {
        let usage = sampleHubUsage()
        let report = HubUsageReport(totals: usage.totals, failovers: usage.failovers,
                                    logins: sampleProviderLogins(),
                                    hostNames: ["work-mbp": "Work MacBook", "trash-can-01": "trash-can-01"])
        // Every login the hub holds, and the removed one with usage; costliest first.
        #expect(report.logins.map(\.pin) == ["work", "codex-ada", "gone", "old"])
        let work = report.logins[0]
        #expect(work.title == "Work")
        #expect(work.amount.turns == 90 && work.amount.tokens == 14_700_000)
        #expect(abs(work.amount.costUsd - 90.05) < 1e-9)
        #expect(work.machines.map(\.name) == ["Work MacBook", "trash-can-01"])
        #expect(work.machines.map(\.amount.turns) == [60, 30])
        #expect(work.models.map(\.name) == ["claude-opus-5-5", "claude-sonnet-5-5"])
        #expect(work.days.map(\.id) == ["2026-11-26", "2026-11-25"])
        #expect(work.days.map(\.name) == ["Thu 26 Nov", "Wed 25 Nov"])
        #expect(work.days[0].amount.turns == 48)
        #expect(HubUsageReport.limits(work) == "3 limit hits · 2 failovers off it")
        let codex = report.logins[1]
        #expect(codex.amount.estimated && UsageReport.dollars(codex.amount) == "≈ $3.40")
        #expect(HubUsageReport.limits(codex) == "2 onto it")
        // A login the hub no longer holds goes by its pin; one with nothing used reads empty.
        #expect(report.logins[2].login == nil && report.logins[2].title == "gone")
        #expect(report.logins[3].amount.turns == 0 && report.logins[3].days.isEmpty)
        #expect(HubUsageReport.limits(report.logins[3]) == nil)
    }

    @Test func choosingAPeriodAsksTheHubForIt() async {
        let hub = FakeUsageHub()
        let usage = hub.model()
        await usage.reload()
        usage.period = .thirtyDays
        await usage.reload()
        #expect(hub.asked == [.week, .thirtyDays])
        guard case .loaded(let answer) = usage.answer else {
            Issue.record("no answer")
            return
        }
        #expect(answer.totals.count == 5)
    }

    @Test func aRefusalShowsAsIs() async {
        let usage = HubUsage(hub: "h-hub") { _ in
            throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "the hub's provider logins are for its owners only"))
        }
        await usage.reload()
        guard case .failed(let why) = usage.answer else {
            Issue.record("expected a failure")
            return
        }
        #expect(why.contains("owners only"))
    }
}

/// Draws the hub's Usage tab as the app's dark appearance shows it, on the Mac, and on an
/// iPhone simulator when the tests run there: every login at a glance, and Work opened to its
/// machines, models and days. Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them
/// there.
@MainActor struct HubUsageScreenshotTests {
    /// Each width drawn, by the name of the device it stands for: on the Mac also an iPhone's,
    /// which CI's Mac runner draws, as its iOS jobs run the UI tests only.
    #if os(macOS)
    private static let widths: [(device: String, width: CGFloat)] = [("mac", 1040), ("iphone", 393)]
    #else
    private static let widths: [(device: String, width: CGFloat)] = [("iphone", 393)]
    #endif

    @Test func drawsTheUsageTab() async throws {
        for (device, width) in Self.widths {
            let fleet = try sampleFleet()
            let logins = FakeLoginsHub().model()
            await logins.reload()
            let usage = FakeUsageHub().model()
            await usage.reload()
            let page = HubPage(fleet: fleet, hub: "h-hub", tab: .usage,
                               secrets: HubSecrets(hub: "h-hub") { _ in .applied }, logins: logins, usage: usage)
            try await shoot(page, width: width, height: width > 500 ? 520 : 900, named: "hub-usage-\(device).png")
        }
    }

    @Test func drawsALoginOpenedToItsMachinesModelsAndDays() async throws {
        for (device, width) in Self.widths {
            let logins = FakeLoginsHub().model()
            await logins.reload()
            let usage = FakeUsageHub().model()
            await usage.reload()
            let section = ScrollView {
                HubUsageSection(usage: usage, logins: logins,
                                hostNames: ["work-mbp": "work-mbp", "trash-can-01": "trash-can-01"], open: ["work"])
                    .padding(20)
            }
            try await shoot(section, width: width, height: width > 500 ? 560 : 1100,
                            named: "hub-usage-open-\(device).png")
        }
    }

    private func sampleFleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.setMachinesForTesting(hubLoginFleetMachines())
        return fleet
    }

    private func shoot(_ content: some View, width: CGFloat, height: CGFloat, named name: String) async throws {
        let root = content.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(Theme.canvas).environment(\.colorScheme, .dark)
        #if os(macOS)
        let host = NSHostingView(rootView: root)
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
        #else
        let controller = UIHostingController(rootView: root)
        controller.overrideUserInterfaceStyle = .dark
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: width, height: height))
        window.overrideUserInterfaceStyle = .dark
        window.rootViewController = controller
        window.makeKeyAndVisible()
        for _ in 0..<5 {
            controller.view.layoutIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let format = UIGraphicsImageRendererFormat()
        format.scale = 3
        // A test has no scene to show the window in: draw its layers.
        let image = UIGraphicsImageRenderer(bounds: window.bounds, format: format).image { context in
            controller.view.layer.render(in: context.cgContext)
        }
        let png = try #require(image.pngData())
        #endif
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}
