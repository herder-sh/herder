import Herder
import SwiftUI

/// A session's details beside its chat: every event that happened, and its statistics.
struct SessionInspector: View {
    let fleet: Fleet
    let key: SessionKey
    @State private var tab = 0

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
                if tab == 0 { events(model) } else { stats(model) }
            }
        }
        .background(Theme.surface)
    }

    private func events(_ model: SessionModel) -> some View {
        ScrollView {
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

    private func stats(_ model: SessionModel) -> some View {
        let stats = model.stats
        let usage = fleet.machines.first { $0.hostId == key.hostId }?.sessionUsage[key.sessionId]
        return ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                group("Session") {
                    row("Created", stats.createdAt?.formatted(date: .abbreviated, time: .shortened) ?? "—")
                    row("Status", model.state.label)
                    row("Model", ModelCatalog.name(model.model ?? "", provider: model.provider))
                    row("Account", model.accountId ?? "—")
                    row("Permissions", model.mode?.label ?? "—")
                    row("Branch", model.branch ?? "—")
                    row("Worktree", model.worktree ?? "—")
                }
                group("Turns") {
                    row("Started", "\(stats.turns)")
                    row("Completed", "\(stats.completed)")
                    row("Interrupted", "\(stats.interrupted)")
                    row("Failed", "\(stats.failed)")
                    row("Time working", Duration.seconds(stats.busy).formatted(.units(allowed: [.hours, .minutes, .seconds], width: .abbreviated)))
                }
                group("Conversation") {
                    row("Prompts", "\(stats.prompts)")
                    row("Replies", "\(stats.replies)")
                    row("Approvals", "\(stats.approvals) · \(stats.allowed) allowed · \(stats.denied) denied")
                    row("Questions", "\(stats.questions)")
                    row("Switches", "\(stats.switches)")
                    row("Pull requests", "\(model.prs.count)")
                }
                if !stats.tools.isEmpty {
                    group("Tools") {
                        ForEach(stats.tools.sorted { $0.value > $1.value }, id: \.key) { name, count in
                            row(name, "\(count)")
                        }
                    }
                }
                if let usage {
                    group("Now on the machine") {
                        row("CPU", "\(Int(usage.cpuPercent.rounded()))%")
                        row("Memory", ByteCountFormatter.string(fromByteCount: Int64(usage.memoryBytes), countStyle: .memory))
                        row("Processes", "\(usage.processes)")
                        ForEach(usage.containers, id: \.id) { container in
                            row(container.name, "\(container.state)")
                        }
                    }
                }
            }
            .padding(14)
        }
    }

    private func group<Content: View>(_ title: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionHeading(title: title)
            content()
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label).foregroundStyle(Theme.secondary)
            Spacer()
            Text(value).foregroundStyle(Theme.text).multilineTextAlignment(.trailing).lineLimit(2).textSelection(.enabled)
        }
        .font(.caption)
    }
}
