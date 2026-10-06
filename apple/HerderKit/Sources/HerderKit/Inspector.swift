import Charts
import Herder
import SwiftUI

/// A session's details beside its chat: what happened, turn by turn, and its statistics.
struct SessionInspector: View {
    let fleet: Fleet
    let key: SessionKey
    @AppStorage("inspectorTab") private var tab = 0
    @AppStorage("inspectorShowAll") private var showAll = false

    var body: some View {
        let model = fleet.sessions[key]
        VStack(spacing: 0) {
            Picker("Details", selection: $tab) {
                Text("Events").tag(0)
                Text("Stats").tag(1)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding(12)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            if let model {
                if tab == 0 { events(model) } else { InspectorStats(fleet: fleet, key: key, model: model) }
            }
        }
        .background(Theme.surface)
    }

    private func events(_ model: SessionModel) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    Text(showAll ? "\(model.timeline.count) events" : "\(model.stats.turns) \(model.stats.turns == 1 ? "turn" : "turns")")
                        .font(.caption)
                        .foregroundStyle(Theme.tertiary)
                    Spacer()
                    Toggle("Show all", isOn: $showAll)
                        .toggleStyle(.switch)
                        .controlSize(.mini)
                        .font(.caption)
                        .foregroundStyle(Theme.secondary)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                if showAll {
                    fullLog(model)
                } else {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        ForEach(model.momentGroups.reversed()) { group in
                            MomentGroupView(group: group, running: group.turnId != nil && group.turnId == model.turn,
                                            fleet: fleet, key: key)
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 12)
                }
            }
        }
    }

    /// Every event, newest first, as the daemon recorded it.
    private func fullLog(_ model: SessionModel) -> some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(model.timeline.reversed()) { entry in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(entry.at.formatted(date: .omitted, time: .shortened))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(Theme.tertiary)
                        .frame(width: 52, alignment: .leading)
                    Text(entry.text)
                        .font(.caption)
                        .foregroundStyle(Theme.text)
                        .lineLimit(3)
                        .textSelection(.enabled)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                Divider().overlay(Theme.stroke)
            }
        }
    }
}

/// A turn's moments under its heading, or the moments between turns, on a rail.
private struct MomentGroupView: View {
    let group: MomentGroup
    /// Whether this is the turn running now.
    let running: Bool
    let fleet: Fleet
    let key: SessionKey

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let turn = group.turn {
                HStack(spacing: 6) {
                    Text("TURN \(turn.number)")
                        .font(.caption2.weight(.semibold))
                        .tracking(0.6)
                        .foregroundStyle(Theme.secondary)
                    Spacer()
                    Text(turn.started, format: .relative(presentation: .named))
                        .font(.caption2)
                        .foregroundStyle(Theme.tertiary)
                        .help(turn.started.formatted(date: .abbreviated, time: .standard))
                }
            }
            VStack(alignment: .leading, spacing: 8) {
                ForEach(group.moments) { MomentRow(moment: $0, fleet: fleet, key: key) }
                if running, let turn = group.turn {
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        MomentLine(symbol: "ellipsis", tint: Theme.running, at: turn.started) {
                            Text("Running · \(InspectorFormat.duration(context.date.timeIntervalSince(turn.started)))")
                                .foregroundStyle(Theme.running)
                        }
                    }
                }
            }
            .background(alignment: .leading) {
                Rectangle().fill(Theme.stroke).frame(width: 1).padding(.leading, 8.5).padding(.vertical, 9)
            }
        }
        .padding(10)
        .background(Theme.background.opacity(group.turn == nil ? 0 : 0.5), in: .rect(cornerRadius: Theme.corner))
        .overlay {
            if group.turn != nil { RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke) }
        }
    }
}

/// One moment: a small coloured icon on the rail, what happened, and when.
private struct MomentRow: View {
    let moment: Moment
    let fleet: Fleet
    let key: SessionKey

    var body: some View {
        switch moment.kind {
        case .created(let branch):
            line("sparkles", Theme.tertiary) { Text(branch.map { "Created on \($0)" } ?? "Created in the folder").foregroundStyle(Theme.secondary) }
        case .prompt(let text, let from):
            line(from == nil ? "person.fill" : "person.2.fill", from == nil ? Theme.running : Theme.merged) {
                Text(text).foregroundStyle(Theme.text).lineLimit(3)
            }
        case .tools(let counts):
            line("wrench.and.screwdriver.fill", Theme.tertiary) {
                Text(Moment.toolLine(counts)).foregroundStyle(Theme.secondary).lineLimit(2)
            }
        case .turnEnded(let end, let duration, let error):
            switch end {
            case .completed:
                line("checkmark", Theme.success) { outcome("Completed", duration, Theme.success) }
            case .interrupted:
                line("stop.fill", Theme.waiting) { outcome("Interrupted", duration, Theme.secondary) }
            case .failed:
                line("xmark", Theme.failure) {
                    VStack(alignment: .leading, spacing: 2) {
                        outcome("Failed", duration, Theme.failure)
                        if let error { Text(error).foregroundStyle(Theme.secondary).lineLimit(2) }
                    }
                }
            }
        case .handoff(let handoff):
            line("arrow.left.arrow.right", Theme.accent) {
                let accounts = fleet.machines.first { $0.hostId == key.hostId }?.accounts ?? []
                let name = { (id: HostId?) in fleet.machineName(id, of: key) }
                HStack(spacing: 6) {
                    HandoffSide(handoff: handoff, side: handoff.from, accounts: accounts, machineName: name)
                        .foregroundStyle(Theme.secondary)
                    Image(systemName: "arrow.right").foregroundStyle(Theme.tertiary)
                    HandoffSide(handoff: handoff, side: handoff.to, accounts: accounts, machineName: name)
                        .foregroundStyle(Theme.accent)
                }
                .fontWeight(.medium)
                .lineLimit(1)
            }
        case .pr(let pr, let change):
            line("arrow.triangle.pull", pr.state.color) {
                Text("**#\(String(pr.number))** \(change) · \(pr.title)").foregroundStyle(Theme.text).lineLimit(2)
            }
        case .prUnlinked(let number):
            line("arrow.triangle.pull", Theme.tertiary) { Text(verbatim: "#\(number) unlinked").foregroundStyle(Theme.secondary) }
        case .approval(let summary):
            line("hand.raised.fill", Theme.accent) { Text(summary).foregroundStyle(Theme.text).lineLimit(2) }
        case .decided(let decision, let byUser):
            let by = byUser ? "" : " by the primary"
            switch decision {
            case .allow: line("checkmark.shield.fill", Theme.success) { Text("Allowed" + by).foregroundStyle(Theme.secondary) }
            case .deny: line("xmark.shield.fill", Theme.failure) { Text("Denied" + by).foregroundStyle(Theme.secondary) }
            case .expired: line("clock", Theme.tertiary) { Text("Approval expired").foregroundStyle(Theme.secondary) }
            }
        case .question(let text):
            line("questionmark.bubble.fill", Theme.accent) { Text(text).foregroundStyle(Theme.text).lineLimit(3) }
        case .answered(let text, let byUser):
            line("text.bubble.fill", Theme.secondary) {
                Text((byUser ? "Answered: " : "The primary answered: ") + text).foregroundStyle(Theme.secondary).lineLimit(2)
            }
        case .spawned(let child, let task):
            line("arrow.branch", Theme.merged) {
                Text("Spawned **\(childName(child, task))**").foregroundStyle(Theme.text).lineLimit(2)
            }
        case .reported(let child, let summary):
            line("arrow.turn.down.left", Theme.merged) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(childName(child, nil)) reported").foregroundStyle(Theme.secondary).lineLimit(1)
                    Text(firstLine(summary)).foregroundStyle(Theme.text).lineLimit(2)
                }
            }
        case .titled(let title):
            line("textformat", Theme.tertiary) { Text("Titled “\(title)”").foregroundStyle(Theme.secondary).lineLimit(2) }
        case .setting(let text):
            line("slider.horizontal.3", Theme.tertiary) { Text(text).foregroundStyle(Theme.secondary).lineLimit(2) }
        }
    }

    private func line(_ symbol: String, _ tint: Color, @ViewBuilder _ content: () -> some View) -> some View {
        MomentLine(symbol: symbol, tint: tint, at: moment.at, content: content)
    }

    private func outcome(_ word: String, _ duration: TimeInterval?, _ tint: Color) -> some View {
        Text(word).foregroundStyle(tint).fontWeight(.medium)
            + Text(duration.map { " · \(InspectorFormat.duration($0))" } ?? "").foregroundStyle(Theme.tertiary)
    }

    private func childName(_ child: SessionId, _ task: String?) -> String {
        fleet.sessions[SessionKey(hostId: key.hostId, sessionId: child)]?.title ?? task ?? String(child.suffix(6))
    }
}

/// The layout of a moment: icon, content, time.
private struct MomentLine<Content: View>: View {
    let symbol: String
    let tint: Color
    let at: Date
    @ViewBuilder let content: Content

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 8, weight: .bold))
                .foregroundStyle(tint)
                .frame(width: 18, height: 18)
                .background(Circle().fill(Theme.surface))
                .background(Circle().fill(tint.opacity(0.18)).padding(-0.5))
            content
                .font(.caption)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 2)
                .textSelection(.enabled)
            Text(at.formatted(date: .omitted, time: .shortened))
                .font(.caption2.monospacedDigit())
                .foregroundStyle(Theme.tertiary)
                .padding(.top, 3)
                .help(at.formatted(date: .abbreviated, time: .standard))
        }
    }
}

enum InspectorFormat {
    /// "42s", "3m 12s", "1h 4m".
    static func duration(_ seconds: TimeInterval) -> String {
        let units: Set<Duration.UnitsFormatStyle.Unit> = seconds >= 3600 ? [.hours, .minutes] : [.minutes, .seconds]
        return Duration.seconds(max(0, seconds.rounded())).formatted(.units(allowed: units, width: .narrow))
    }
}

/// A session's numbers: tiles for the key ones, charts for where the time and the calls went,
/// and the session's settings.
private struct InspectorStats: View {
    let fleet: Fleet
    let key: SessionKey
    let model: SessionModel

    var body: some View {
        let stats = model.stats
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    tiles(stats, now: context.date)
                }
                if !stats.turnLog.isEmpty { turns(stats) }
                if stats.createdAt != nil {
                    TimelineView(.periodic(from: .now, by: 30)) { context in time(stats, now: context.date) }
                }
                if !stats.tools.isEmpty { tools(stats) }
                if stats.approvals + stats.questions > 0 { requests(stats) }
                if !stats.modelUse.isEmpty { models(stats) }
                details(stats)
                if let usage = fleet.machines.first(where: { $0.hostId == key.hostId })?.sessionUsage[key.sessionId] {
                    section("Now on the machine") {
                        grid {
                            pair("CPU", "\(Int(usage.cpuPercent.rounded()))%")
                            pair("Memory", ByteCountFormatter.string(fromByteCount: Int64(usage.memoryBytes), countStyle: .memory))
                            pair("Processes", "\(usage.processes)")
                            ForEach(usage.containers, id: \.id) { pair($0.name, "\($0.state)") }
                        }
                    }
                }
            }
            .padding(14)
        }
    }

    private func tiles(_ stats: SessionStats, now: Date) -> some View {
        HStack(spacing: 6) {
            tile("Turns", "\(stats.turns)", Theme.running)
            tile("Working", InspectorFormat.duration(model.runTime(at: now)), Theme.success)
            tile("Prompts", "\(stats.prompts)", Theme.accent)
            tile("Tools", "\(stats.tools.values.reduce(0, +))", Theme.merged)
        }
    }

    private func tile(_ label: String, _ value: String, _ tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value)
                .font(.callout.weight(.semibold).monospacedDigit())
                .foregroundStyle(Theme.text)
            Text(label).font(.caption2).foregroundStyle(Theme.tertiary)
        }
        .lineLimit(1)
        .minimumScaleFactor(0.7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 7)
        .padding(.top, 9)
        .padding(.bottom, 7)
        .background(Theme.raised.opacity(0.6), in: .rect(cornerRadius: 8))
        .overlay(alignment: .top) { Capsule().fill(tint).frame(height: 2).padding(.horizontal, 7) }
    }

    /// Each turn's length, coloured by how it ended; the last 40.
    private func turns(_ stats: SessionStats) -> some View {
        let shown = stats.turnLog.suffix(40)
        return section("Turns", trailing: "\(stats.completed) done · \(stats.interrupted) stopped · \(stats.failed) failed") {
            Chart(shown) { turn in
                BarMark(x: .value("Turn", "\(turn.number)"),
                        y: .value("Minutes", (turn.duration ?? model.runTime(at: .now) - stats.busy) / 60))
                    .foregroundStyle(by: .value("Ended", Self.word(turn.end)))
                    .cornerRadius(2)
            }
            .chartForegroundStyleScale(domain: Self.ends.map(\.0), range: Self.ends.map(\.1))
            .chartLegend(.hidden)
            .chartXAxis(.hidden)
            .chartYAxis {
                AxisMarks(position: .leading, values: .automatic(desiredCount: 3)) { value in
                    AxisGridLine().foregroundStyle(Theme.stroke)
                    AxisValueLabel { if let minutes = value.as(Double.self) { Text("\(minutes, specifier: "%.0f")m") } }
                        .foregroundStyle(Theme.tertiary)
                }
            }
            .frame(height: 90)
        }
    }

    private static let ends: [(String, Color)] = [
        ("Completed", Theme.success), ("Interrupted", Theme.waiting), ("Failed", Theme.failure), ("Running", Theme.running),
    ]

    private static func word(_ end: TurnEnd?) -> String {
        switch end {
        case .completed: "Completed"
        case .interrupted: "Interrupted"
        case .failed: "Failed"
        case nil: "Running"
        }
    }

    /// The session's life split into time in turns and time between them.
    private func time(_ stats: SessionStats, now: Date) -> some View {
        let working = model.runTime(at: now)
        let life = max(working, now.timeIntervalSince(stats.createdAt ?? now))
        let idle = life - working
        return section("Time", trailing: life > 0 ? "\(Int((working / life * 100).rounded()))% working" : nil) {
            VStack(alignment: .leading, spacing: 6) {
                GeometryReader { proxy in
                    HStack(spacing: 2) {
                        RoundedRectangle(cornerRadius: 3).fill(Theme.success)
                            .frame(width: life > 0 ? max(3, proxy.size.width * working / life) : 0)
                        RoundedRectangle(cornerRadius: 3).fill(Theme.idle)
                    }
                }
                .frame(height: 10)
                HStack(spacing: 12) {
                    legend("Working", InspectorFormat.duration(working), Theme.success)
                    legend("Idle", InspectorFormat.duration(idle), Theme.idle)
                }
            }
        }
    }

    /// The most used tools, longest bar first.
    private func tools(_ stats: SessionStats) -> some View {
        let ranked = stats.toolRanking
        let shown = Array(ranked.prefix(8))
        return section("Tools", trailing: ranked.count > shown.count ? "top 8 of \(ranked.count)" : nil) {
            HStack(spacing: 8) {
                VStack(alignment: .trailing, spacing: 0) {
                    ForEach(shown, id: \.name) { tool in
                        Text(tool.name).frame(height: 20).lineLimit(1).truncationMode(.middle)
                    }
                }
                .font(.caption2)
                .foregroundStyle(Theme.secondary)
                .frame(maxWidth: 90, alignment: .trailing)
                .fixedSize(horizontal: true, vertical: false)
                Chart(shown, id: \.name) { tool in
                    BarMark(x: .value("Calls", tool.count), y: .value("Tool", tool.name), height: .fixed(10))
                        .foregroundStyle(ToolKind(tool.name).color.gradient)
                        .cornerRadius(3)
                        .annotation(position: .trailing, spacing: 4) {
                            Text("\(tool.count)").font(.caption2.monospacedDigit()).foregroundStyle(Theme.tertiary)
                        }
                }
                .chartXAxis(.hidden)
                .chartYAxis(.hidden)
                .chartXScale(range: .plotDimension(endPadding: 24))
                .frame(height: CGFloat(shown.count) * 20)
            }
        }
    }

    /// Approvals by decision, and questions.
    private func requests(_ stats: SessionStats) -> some View {
        let parts: [(String, Int, Color)] = [
            ("Allowed", stats.allowed, Theme.success), ("Denied", stats.denied, Theme.failure),
            ("Expired", stats.expired, Theme.idle),
            ("Pending", max(0, stats.approvals - stats.allowed - stats.denied - stats.expired), Theme.accent),
        ].filter { $0.1 > 0 }
        return section("Requests", trailing: stats.questions > 0 ? "\(stats.questions) \(stats.questions == 1 ? "question" : "questions")" : nil) {
            VStack(alignment: .leading, spacing: 6) {
                if stats.approvals > 0 {
                    GeometryReader { proxy in
                        HStack(spacing: 2) {
                            ForEach(parts, id: \.0) { part in
                                RoundedRectangle(cornerRadius: 3).fill(part.2)
                                    .frame(width: max(3, (proxy.size.width - CGFloat(parts.count - 1) * 2) * CGFloat(part.1) / CGFloat(stats.approvals)))
                            }
                        }
                    }
                    .frame(height: 10)
                }
                HStack(spacing: 12) {
                    legend("Approvals", "\(stats.approvals)", Theme.tertiary)
                    ForEach(parts, id: \.0) { legend($0.0, "\($0.1)", $0.2) }
                }
            }
        }
    }

    /// Time in turns by model.
    private func models(_ stats: SessionStats) -> some View {
        let uses = stats.modelUse
        let longest = uses.first?.time ?? 1
        return section("Models") {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(uses, id: \.self) { use in
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 5) {
                            if let provider = use.provider { ProviderMark(provider: provider, size: 11) }
                            Text(ModelCatalog.name(use.model ?? "", provider: use.provider)).lineLimit(1)
                            Spacer()
                            Text("\(use.turns) \(use.turns == 1 ? "turn" : "turns") · \(InspectorFormat.duration(use.time))")
                                .foregroundStyle(Theme.tertiary).monospacedDigit()
                        }
                        .font(.caption)
                        .foregroundStyle(Theme.secondary)
                        GeometryReader { proxy in
                            RoundedRectangle(cornerRadius: 2).fill(Theme.secondary.opacity(0.55))
                                .frame(width: max(3, proxy.size.width * use.time / max(longest, 1)))
                        }
                        .frame(height: 4)
                    }
                }
            }
        }
    }

    private func details(_ stats: SessionStats) -> some View {
        section("Session") {
            grid {
                pair("Created", stats.createdAt?.formatted(date: .abbreviated, time: .shortened) ?? "—")
                pair("Status", model.state.label)
                pair("Model", ModelCatalog.name(model.model ?? "", provider: model.provider))
                pair("Account", model.accountId ?? "—")
                pair("Permissions", model.mode?.label ?? "—")
                pair("Branch", model.branch ?? "—")
                pair("Worktree", model.worktree.map { ($0 as NSString).abbreviatingWithTildeInPath } ?? "—")
                pair("Replies", "\(stats.replies)")
                pair("Handoffs", "\(stats.switches)")
                pair("Pull requests", "\(model.prs.count)")
            }
        }
    }

    private func section(_ title: String, trailing: String? = nil, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                SectionHeading(title: title)
                Spacer()
                if let trailing { Text(trailing).font(.caption2).foregroundStyle(Theme.tertiary).lineLimit(1) }
            }
            content()
        }
    }

    private func grid(@ViewBuilder _ rows: () -> some View) -> some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 10, verticalSpacing: 4) { rows() }
            .font(.caption)
    }

    private func pair(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).foregroundStyle(Theme.tertiary)
            Text(value).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .help(value)
        }
    }

    private func legend(_ label: String, _ value: String, _ tint: Color) -> some View {
        HStack(spacing: 4) {
            Circle().fill(tint).frame(width: 6, height: 6)
            Text(label).foregroundStyle(Theme.tertiary)
            Text(value).foregroundStyle(Theme.secondary).monospacedDigit()
        }
        .font(.caption2)
        .lineLimit(1)
    }
}

/// What a tool does, by its name across providers, for its colour in the Tools chart.
enum ToolKind: Hashable {
    case shell, edit, read, search, web, agent, other

    init(_ name: String) {
        let name = name.lowercased()
        func any(_ words: [String]) -> Bool { words.contains { name.contains($0) } }
        self = if any(["bash", "shell", "exec", "command"]) { .shell }
            else if any(["edit", "write", "patch"]) { .edit }
            else if any(["web", "fetch", "browse"]) { .web }
            else if any(["grep", "glob", "search", "find", "list", "ls"]) { .search }
            else if any(["read", "view", "open"]) { .read }
            else if any(["task", "agent", "spawn"]) { .agent }
            else { .other }
    }

    var color: Color {
        switch self {
        case .shell: Theme.accent
        case .edit: Theme.success
        case .read: Theme.running
        case .search: Color(light: 0x4A8FB8, dark: 0x56B4E9)
        case .web: Theme.merged
        case .agent: Theme.failure
        case .other: Theme.idle
        }
    }
}
