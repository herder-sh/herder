import Herder
import SwiftUI

/// Usage: the tokens and API-equivalent dollars of the turns the machines ran over a period,
/// overall, per account with its plan windows, and per model; all machines added up, or one.
struct UsageView: View {
    let fleet: Fleet
    @State private var period = UsagePeriod.week
    /// The one machine shown; `nil` adds every machine up.
    @State private var hostId: HostId?
    @State private var answers: [MachineUsage] = []
    @State private var failures: [String] = []
    @State private var loaded = false

    /// What the answers depend on: asked again when the period or the connected machines change.
    private struct Question: Equatable {
        let period: UsagePeriod
        let machines: [HostId]
    }

    private var hosts: [MachineSummary] {
        fleet.lists.machines.filter { $0.connected && $0.hosts.isEmpty }
    }

    var body: some View {
        let report = UsageReport(answers, only: hostId)
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                controls
                if !loaded {
                    ProgressView().frame(maxWidth: .infinity)
                } else if hosts.isEmpty {
                    Text("No machine is connected.").font(.footnote).foregroundStyle(Theme.tertiary)
                } else {
                    UsageTotals(amount: report.total)
                    accounts(report.accounts)
                    models(report.models)
                }
                ForEach(failures, id: \.self) { failure in
                    Text(failure).font(.footnote).foregroundStyle(Theme.failure)
                }
            }
            .frame(maxWidth: 760, alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.vertical, 16)
            .frame(maxWidth: .infinity)
        }
        .background(Theme.background)
        .navigationTitle("Usage")
        .refreshable { await load() }
        .task(id: Question(period: period, machines: hosts.map(\.hostId))) { await load() }
    }

    private func load() async {
        let (answers, failures) = await fleet.usage(over: period)
        self.answers = answers
        self.failures = failures
        loaded = true
    }

    private var controls: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) {
                periodPicker.frame(maxWidth: 320)
                Spacer()
                machineMenu
            }
            VStack(alignment: .leading, spacing: 10) {
                periodPicker
                machineMenu
            }
        }
    }

    private var periodPicker: some View {
        Picker("Period", selection: $period) {
            ForEach(UsagePeriod.all, id: \.self) { Text($0.label).tag($0) }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .accessibilityLabel(period.title)
    }

    private var machineMenu: some View {
        Menu {
            Button("All Machines") { hostId = nil }
            Divider()
            ForEach(hosts) { machine in
                Button(machine.name) { hostId = machine.hostId }
            }
        } label: {
            Chip(symbol: "server.rack",
                 text: hosts.first { $0.hostId == hostId }?.name ?? "All Machines")
                .frame(minHeight: 30)
                .hitTarget()
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .fixedSize()
    }

    @ViewBuilder private func accounts(_ rows: [UsageReport.AccountRow]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeading(title: "Accounts", count: rows.count)
            if rows.isEmpty {
                Text("No turns in this period.").font(.footnote).foregroundStyle(Theme.tertiary)
            }
            ForEach(rows) { UsageAccountRow(row: $0, showsMachine: hostId == nil && hosts.count > 1) }
        }
    }

    @ViewBuilder private func models(_ rows: [UsageReport.ModelRow]) -> some View {
        if !rows.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                SectionHeading(title: "By Model", count: rows.count)
                Card(padding: 0) {
                    VStack(spacing: 0) {
                        UsageModelLine(provider: nil, model: "Model", turns: "Turns", tokens: "Tokens",
                                       dollars: "Cost", heading: true)
                        ForEach(rows) { row in
                            Rectangle().fill(Theme.stroke).frame(height: 1)
                            UsageModelLine(provider: row.provider, model: row.model.isEmpty ? "Default" : row.model,
                                           turns: "\(row.amount.turns)", tokens: UsageReport.tokens(row.amount.tokens),
                                           dollars: UsageReport.dollars(row.amount))
                        }
                    }
                }
            }
        }
    }
}

/// The period's totals as a grid of tiles.
private struct UsageTotals: View {
    let amount: UsageAmount

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 10)], spacing: 10) {
            tile("Cost", UsageReport.dollars(amount),
                 amount.estimated ? "API prices, partly estimated" : "At API prices")
            tile("Tokens", UsageReport.tokens(amount.tokens), amount.turns == 1 ? "1 turn" : "\(amount.turns) turns")
            tile("Cache savings", amount.cacheSavings.map { "\(Int(($0 * 100).rounded()))%" } ?? "–",
                 "of input read from the cache")
            tile("Input", UsageReport.tokens(amount.input), "sent afresh")
            tile("Output", UsageReport.tokens(amount.output), "reasoning included")
            tile("Cache read", UsageReport.tokens(amount.cacheRead), "cache write \(UsageReport.tokens(amount.cacheWrite))")
        }
    }

    private func tile(_ title: String, _ value: String, _ detail: String) -> some View {
        Card(padding: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.caption.weight(.medium)).foregroundStyle(Theme.secondary)
                Text(value).font(.title3.weight(.semibold)).monospacedDigit().foregroundStyle(Theme.text)
                    .lineLimit(1).minimumScaleFactor(0.7)
                Text(detail).font(.caption2).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// One account: its provider, tokens and dollars for the period, and how much of its plan's
/// session and weekly windows is left.
private struct UsageAccountRow: View {
    let row: UsageReport.AccountRow
    let showsMachine: Bool

    var body: some View {
        Card(padding: 12) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 10) {
                    ProviderMark(provider: row.provider, size: 16)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.label).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
                        Text(([ModelCatalog.providerName(row.provider)] + (showsMachine ? [row.machine] : [])).joined(separator: " · "))
                            .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                    Spacer()
                    VStack(alignment: .trailing, spacing: 2) {
                        Text(UsageReport.dollars(row.amount)).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        Text("\(UsageReport.tokens(row.amount.tokens)) tokens").font(.caption).foregroundStyle(Theme.tertiary)
                    }
                    .monospacedDigit()
                    .fixedSize()
                }
                if let session = row.session { window("Session", session) }
                if let weekly = row.weekly { window("Weekly", weekly) }
            }
        }
    }

    private func window(_ label: String, _ window: WindowLeft) -> some View {
        HStack(spacing: 10) {
            Text(label).frame(width: 64, alignment: .leading).foregroundStyle(Theme.secondary)
            UsageBar(percent: window.percentUsed)
            Text("\(Int(window.percentLeft.rounded()))% left").frame(width: 64, alignment: .trailing)
                .foregroundStyle(Theme.text)
            Text(window.resets.isEmpty ? "" : "resets \(window.resets)").frame(width: 92, alignment: .trailing)
                .foregroundStyle(Theme.tertiary)
        }
        .font(.caption.monospacedDigit())
        .lineLimit(1)
    }
}

/// One line of the by-model table; the heading when `heading`.
private struct UsageModelLine: View {
    let provider: Provider?
    let model: String
    let turns: String
    let tokens: String
    let dollars: String
    var heading = false

    var body: some View {
        HStack(spacing: 10) {
            if let provider { ProviderMark(provider: provider, size: 13).frame(width: 16) } else { Spacer().frame(width: 16) }
            Text(model).font(heading ? .caption.weight(.semibold) : Theme.monoSmall)
                .lineLimit(1).truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text(turns).frame(width: 48, alignment: .trailing)
            Text(tokens).frame(width: 64, alignment: .trailing)
            Text(dollars).frame(width: 72, alignment: .trailing)
        }
        .font(heading ? .caption.weight(.semibold) : .caption.monospacedDigit())
        .foregroundStyle(heading ? Theme.secondary : Theme.text)
        .padding(.horizontal, 12)
        .padding(.vertical, heading ? 8 : 10)
    }
}
