import Foundation
import Herder
import Observation
import SwiftUI

/// What each of a hub's logins was used for over a period, on every machine, as the hub adds
/// it up from the sessions its hosts replicate: one row per login, each with its machines,
/// models and days.
struct HubUsageReport: Equatable {
    /// A machine, model or day of a login, with what it used there.
    struct Part: Equatable, Identifiable {
        let id: String
        let name: String
        var amount = UsageAmount()
    }

    struct Login: Equatable, Identifiable {
        var id: String { pin }
        let pin: String
        let provider: Provider
        /// The owner's name for it, else its email; its pin once the hub no longer holds it.
        let title: String
        /// The login as the hub lists it now; nil for one it no longer holds.
        let login: ProviderLogin?
        var amount = UsageAmount()
        var limitHits: UInt64 = 0
        var failoversOut: UInt64 = 0
        var failoversIn: UInt64 = 0
        /// Most expensive first.
        var machines: [Part] = []
        var models: [Part] = []
        /// Newest first.
        var days: [Part] = []
    }

    /// Every login the hub holds, and any other with usage in the period; most expensive first,
    /// then by title.
    let logins: [Login]

    init(totals: [HubUsageTotal], failovers: [HubFailoverTotal], logins: [ProviderLogin], hostNames: [HostId: String]) {
        var rows: [String: Login] = [:]
        for login in logins {
            rows[login.pin] = Login(pin: login.pin, provider: login.provider, title: login.label ?? login.name, login: login)
        }
        var machines: [String: [String: Part]] = [:]
        var models: [String: [String: Part]] = [:]
        var days: [String: [String: Part]] = [:]
        for total in totals {
            let pin = total.pin
            rows[pin, default: Login(pin: pin, provider: total.usage.provider, title: pin, login: nil)].amount.add(total.usage)
            machines[pin, default: [:]][total.hostId, default: Part(id: total.hostId, name: hostNames[total.hostId] ?? total.hostId)]
                .amount.add(total.usage)
            let model = total.usage.model.isEmpty ? "Default" : total.usage.model
            models[pin, default: [:]][model, default: Part(id: model, name: model)].amount.add(total.usage)
            days[pin, default: [:]][total.day, default: Part(id: total.day, name: Self.day(total.day))].amount.add(total.usage)
        }
        for failover in failovers {
            let pin = failover.pin
            guard rows[pin] != nil else { continue }
            rows[pin]?.limitHits += failover.failovers.limitHits
            rows[pin]?.failoversOut += failover.failovers.failoversOut
            rows[pin]?.failoversIn += failover.failovers.failoversIn
        }
        for pin in rows.keys {
            rows[pin]?.machines = Self.costliest(machines[pin])
            rows[pin]?.models = Self.costliest(models[pin])
            rows[pin]?.days = (days[pin] ?? [:]).values.sorted { $0.id > $1.id }
        }
        self.logins = rows.values.sorted { a, b in
            if a.amount.costUsd != b.amount.costUsd { return a.amount.costUsd > b.amount.costUsd }
            if a.amount.tokens != b.amount.tokens { return a.amount.tokens > b.amount.tokens }
            return a.title.localizedStandardCompare(b.title) == .orderedAscending
        }
    }

    private static func costliest(_ parts: [String: Part]?) -> [Part] {
        (parts ?? [:]).values.sorted { a, b in
            if a.amount.costUsd != b.amount.costUsd { return a.amount.costUsd > b.amount.costUsd }
            if a.amount.tokens != b.amount.tokens { return a.amount.tokens > b.amount.tokens }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    /// A UTC day, `2026-11-26`, as "Thu 26 Nov".
    static func day(_ day: String) -> String {
        let parse = DateFormatter()
        parse.locale = Locale(identifier: "en_US_POSIX")
        parse.timeZone = TimeZone(identifier: "UTC")
        parse.dateFormat = "yyyy-MM-dd"
        guard let date = parse.date(from: day) else { return day }
        let show = DateFormatter()
        show.locale = Locale(identifier: "en_GB")
        show.timeZone = TimeZone(identifier: "UTC")
        show.dateFormat = "EEE d MMM"
        return show.string(from: date)
    }

    /// "3 limit hits · 1 failover": what stood out about `login`; nil when nothing did.
    static func limits(_ login: Login) -> String? {
        var parts: [String] = []
        if login.limitHits > 0 { parts.append("\(login.limitHits) limit hit\(login.limitHits == 1 ? "" : "s")") }
        let moved = login.failoversOut
        if moved > 0 { parts.append("\(moved) failover\(moved == 1 ? "" : "s") off it") }
        if login.failoversIn > 0 { parts.append("\(login.failoversIn) onto it") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// A hub's answer to what its logins used over a period, asked again when the period changes.
@MainActor @Observable
final class HubUsage {
    typealias Send = HubLogins.Send

    struct Answer: Equatable {
        let totals: [HubUsageTotal]
        let failovers: [HubFailoverTotal]
    }

    let hub: HostId
    private let send: Send
    var period: UsagePeriod = .week
    private(set) var answer: Load<Answer> = .asking

    init(hub: HostId, send: @escaping Send) {
        self.hub = hub
        self.send = send
    }

    convenience init(fleet: Fleet, hub: HostId) {
        let client = fleet.client
        self.init(hub: hub) { try await client.send(hostId: hub, command: $0) }
    }

    /// The periods the dashboard offers.
    static let periods: [UsagePeriod] = [.day, .week, .thirtyDays]

    func reload() async {
        do {
            guard case .hubUsage(_, _, let totals, let failovers) = try await send(.getHubUsage(period: period)) else {
                throw HerderError.Local(detail: "the hub did not add its logins' usage up")
            }
            answer = .loaded(Answer(totals: totals, failovers: failovers))
        } catch {
            answer = .failed(describe(error))
        }
    }
}

/// The hub's Usage tab: one row per login, what it used over the period on every machine next
/// to what is left of its limits; a row opens to its machines, models and days.
struct HubUsageSection: View {
    @Bindable var usage: HubUsage
    let logins: HubLogins
    let hostNames: [HostId: String]
    /// The logins opened to their machines, models and days, by pin.
    @State private var open: Set<String>

    init(usage: HubUsage, logins: HubLogins, hostNames: [HostId: String], open: Set<String> = []) {
        _usage = Bindable(usage)
        self.logins = logins
        self.hostNames = hostNames
        _open = State(initialValue: open)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 4) {
                    heading
                    Spacer(minLength: 12)
                    periods
                }
                VStack(alignment: .leading, spacing: 10) {
                    heading
                    periods
                }
            }
            switch (usage.answer, logins.list) {
            case (.failed(let why), _), (_, .failed(let why)):
                Text(why).font(.subheadline).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("hub-usage-error")
            case (.loaded(let answer), .loaded(let list)):
                let report = HubUsageReport(totals: answer.totals, failovers: answer.failovers, logins: list,
                                            hostNames: hostNames)
                if report.logins.isEmpty {
                    Text("No logins yet. Add one on the Logins tab, and what sessions use on it adds up here.")
                        .font(.subheadline).foregroundStyle(Theme.tertiary)
                } else {
                    VStack(spacing: 0) {
                        ForEach(Array(report.logins.enumerated()), id: \.element.id) { index, login in
                            if index > 0 { RowDivider() }
                            HubUsageRow(login: login, open: open.contains(login.pin)) {
                                if open.contains(login.pin) { open.remove(login.pin) } else { open.insert(login.pin) }
                            }
                        }
                    }
                    .background(.card, in: .rect(cornerRadius: Theme.corner))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
                }
            default:
                ProgressView().frame(maxWidth: .infinity)
            }
        }
        .task(id: usage.period) { await usage.reload() }
    }

    private var heading: some View {
        HStack(spacing: 4) {
            SectionHeading(title: "Usage by login")
            InfoButton(text: "What sessions on every machine used on each of this hub's logins, added up on the hub from the sessions they back up to it. Dollars are what the tokens cost at the provider's API prices. The limits are each login's own, as the provider last reported them.")
        }
    }

    private var periods: some View {
        Picker("Period", selection: $usage.period) {
            ForEach(HubUsage.periods, id: \.self) { Text(Self.short($0)).tag($0) }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .fixedSize()
        .accessibilityIdentifier("hub-usage-period")
    }

    static func short(_ period: UsagePeriod) -> String {
        switch period {
        case .day: "24 h"
        case .week: "7 d"
        case .thirtyDays: "30 d"
        case .month: "Month"
        }
    }
}

/// A login: its name and provider, what it used, what is left of its limits; opened, what it
/// used on each machine, with each model and on each day.
private struct HubUsageRow: View {
    let login: HubUsageReport.Login
    let open: Bool
    let toggle: () -> Void

    var body: some View {
        let windows: (session: WindowLeft?, weekly: WindowLeft?) =
            login.login.map { HubLogins.windows($0) } ?? (session: nil, weekly: nil)
        VStack(alignment: .leading, spacing: 8) {
            Button(action: toggle) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 10) {
                        ProviderMark(provider: login.provider, size: 14).frame(width: 16)
                        title
                        Spacer(minLength: 8)
                        amount(login.amount)
                        Image(systemName: open ? "chevron.down" : "chevron.right")
                            .font(.caption2.weight(.semibold)).foregroundStyle(Theme.tertiary)
                            .frame(width: 10)
                    }
                    ViewThatFits(in: .horizontal) {
                        HStack(spacing: 16) {
                            limits(windows)
                            note
                            Spacer(minLength: 0)
                        }
                        VStack(alignment: .leading, spacing: 6) {
                            limits(windows)
                            note
                        }
                    }
                    .font(.caption)
                    .padding(.leading, 26)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("hub-usage-\(login.pin)")
            if open { parts }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    private var title: some View {
        HStack(spacing: 6) {
            Text(login.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                .lineLimit(1).truncationMode(.middle)
            if login.login?.fallback == true { Badge(text: "Fallback") }
            if login.login == nil { Badge(text: "Removed") }
        }
    }

    private func amount(_ amount: UsageAmount) -> some View {
        HStack(spacing: 10) {
            if amount.turns == 0 {
                Text("—").foregroundStyle(Theme.tertiary)
            } else {
                Text(UsageReport.tokens(amount.tokens)).foregroundStyle(Theme.secondary)
                    .frame(minWidth: 56, alignment: .trailing)
                Text(UsageReport.dollars(amount)).foregroundStyle(Theme.text)
                    .frame(minWidth: 64, alignment: .trailing)
            }
        }
        .font(.caption.monospacedDigit())
    }

    @ViewBuilder private func limits(_ windows: (session: WindowLeft?, weekly: WindowLeft?)) -> some View {
        if let session = windows.session { limit("5-hour", session) }
        if let weekly = windows.weekly { limit("Weekly", weekly) }
        if windows.session == nil && windows.weekly == nil {
            Text(login.login == nil ? "No longer on the hub" : "No limits reported").foregroundStyle(Theme.tertiary)
        }
    }

    @ViewBuilder private var note: some View {
        if let limits = HubUsageReport.limits(login) {
            Text(limits).foregroundStyle(Theme.tertiary).lineLimit(1)
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

    /// Machines, models and days side by side where they fit, else one under another.
    private var parts: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .top, spacing: 24) {
                list("Machines", login.machines)
                list("Models", login.models)
                list("Days", login.days)
            }
            VStack(alignment: .leading, spacing: 12) {
                list("Machines", login.machines)
                list("Models", login.models)
                list("Days", login.days)
            }
        }
        .padding(.leading, 26)
        .padding(.bottom, 4)
    }

    private func list(_ title: String, _ parts: [HubUsageReport.Part]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
            if parts.isEmpty {
                Text("None in this period").font(.caption).foregroundStyle(Theme.tertiary)
            }
            ForEach(parts) { part in
                HStack(spacing: 10) {
                    Text(part.name).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                        .frame(minWidth: 120, maxWidth: 220, alignment: .leading)
                    Text(UsageReport.tokens(part.amount.tokens)).foregroundStyle(Theme.secondary)
                        .frame(width: 56, alignment: .trailing)
                    Text(UsageReport.dollars(part.amount)).frame(width: 64, alignment: .trailing)
                }
                .font(.caption.monospacedDigit())
            }
        }
        .frame(maxWidth: 360, alignment: .leading)
    }
}
