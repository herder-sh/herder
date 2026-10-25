import Foundation
import Herder
@testable import HerderKit

/// The fleet of mockup §3: three machines, a Claude account signed out on one, CLIs behind,
/// updating, queued and failed.
enum ProvidersFixture {
    static func overview(now: Date = .now) -> ProvidersOverview {
        let resets = ISO8601DateFormatter().string(from: now.addingTimeInterval(2 * 3_600 + 20 * 60))
        let weekResets = ISO8601DateFormatter().string(from: now.addingTimeInterval(4 * 86_400))
        func account(_ id: AccountId, _ provider: Provider, email: String?, session: Double? = nil, week: Double? = nil,
                     signedOut: Date? = nil) -> Account {
            var usage: [UsageWindow] = []
            if let session { usage.append(UsageWindow(window: "five_hour", usedPercent: session, resetsAt: resets)) }
            if let week { usage.append(UsageWindow(window: "seven_day", usedPercent: week, resetsAt: weekResets)) }
            return Account(accountId: id, provider: provider, label: id, configDir: nil, email: email, usage: usage,
                           needsLogin: signedOut.map { ISO8601DateFormatter().string(from: $0) })
        }
        func cli(_ provider: Provider, _ version: String?, latest: String? = nil, canInstall: Bool = true) -> ProviderStatus {
            ProviderStatus(provider: provider, installed: version != nil, version: version, latestVersion: latest,
                           binary: nil, canInstall: canInstall, canUpdate: version != nil)
        }
        let work = account("work", "claude", email: "work@example.com", session: 82, week: 36)
        let codex = account("pro", "codex", email: "dev@example.com", session: 9, week: 23)
        let machines = [
            machine("a", name: "work-mbp", sessions: [], accounts: [work, codex, account("cursor", "cursor", email: nil)],
                    providers: [cli("claude", "2.1.310", latest: "2.1.310"), cli("codex", "0.50.0", latest: "0.51.2"),
                                cli("grok", "0.9.4"), cli("opencode", nil)]),
            machine("b", name: "trash-can-01", sessions: [], accounts: [
                work, codex, account("ops", "claude", email: "ops@example.com", signedOut: now.addingTimeInterval(-14 * 60)),
            ], providers: [cli("claude", "2.1.303", latest: "2.1.310"), cli("codex", "0.50.0", latest: "0.51.2"),
                           cli("grok", nil), cli("opencode", nil)]),
            machine("c", name: "draugelis", sessions: [], accounts: [],
                    providers: [cli("claude", "2.1.310"), cli("codex", "0.51.2"), cli("grok", nil, canInstall: false),
                                cli("opencode", "1.4.0")]),
        ]
        return ProvidersOverview(accounts: ProviderAccounts(machines: machines, now: now),
                                 versions: ProviderVersions(machines: machines), installs: [
            JobKey(hostId: "a", kind: .install("codex")): .installing,
            JobKey(hostId: "b", kind: .install("codex")): .queued,
            JobKey(hostId: "a", kind: .install("grok")): .updated(from: "0.9.1", to: "0.9.4"),
            JobKey(hostId: "b", kind: .install("opencode")): .failed("npm: EACCES on /usr/local/lib · exit 243"),
        ])
    }

    /// The week's cost of each account.
    static let report = UsageReport([
        MachineUsage(hostId: "a", name: "work-mbp", accounts: [
            Account(accountId: "work", provider: "claude", label: "work", configDir: nil, email: "work@example.com", usage: []),
            Account(accountId: "pro", provider: "codex", label: "pro", configDir: nil, email: "dev@example.com", usage: []),
        ], totals: [
            total("work", "claude", "claude-opus-4-1", 412.3),
            total("pro", "codex", "gpt-5-codex", 131.1),
        ], failovers: []),
        MachineUsage(hostId: "b", name: "trash-can-01", accounts: [
            Account(accountId: "ops", provider: "claude", label: "ops", configDir: nil, email: "ops@example.com", usage: []),
        ], totals: [total("ops", "claude", "claude-sonnet-4-5", 88.4)], failovers: []),
    ])

    private static func total(_ id: AccountId, _ provider: Provider, _ model: String, _ cost: Double) -> UsageTotal {
        UsageTotal(accountId: id, provider: provider, model: model, turns: 40, input: 1_000_000, output: 200_000,
                   cacheRead: 0, cacheWrite: 0, costUsd: cost, costEstimated: true)
    }
}
