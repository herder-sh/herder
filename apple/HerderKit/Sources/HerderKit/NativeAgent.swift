import Foundation
import Herder
import SwiftUI

/// A provider-owned agent is a nested conversation within a turn, not a new session.
struct NativeAgent: Hashable, Identifiable {
    struct ID: Hashable, Identifiable {
        let turnId: TurnId
        let callId: ItemId
        var id: Self { self }
    }

    let id: ID
    let title: String
    /// The kind of agent the provider ran, such as "Explore".
    let kind: String?
    /// The model the agent ran on, when the call chose one.
    let model: String?
    let prompt: String
    let outcome: ToolCall.Outcome
    /// What the agent returned. Never a background launch's acknowledgement: that is metadata
    /// for the parent model (an agent id, an output file), not the agent's work.
    let result: String?
    let background: Bool
    /// The provider acknowledged a background launch; the agent works on after its call returned.
    let launched: Bool
    let startedAt: Date?
    let endedAt: Date?

    static func isAgent(_ name: String) -> Bool {
        ["agent", "task"].contains(name.lowercased())
    }

    /// Whether a tool result acknowledges a background launch, as Claude's does for an agent it
    /// runs in the background whether or not the call asked for `run_in_background`.
    static func isLaunch(_ output: String) -> Bool {
        output.hasPrefix("Async agent launched") || output.contains("\nagentId: ") && output.contains("\noutput_file: ")
    }

    /// `working` is whether the session is running: the daemon keeps it so while background
    /// agents work, between turns too.
    init(item: Item, items: [Item], runningTurn: TurnId?, working: Bool = false, streaming: [Item] = [],
         times: [String: Date] = [:]) {
        id = ID(turnId: item.turnId, callId: item.id)
        var input: [String: Any] = [:]
        if case .toolCall(_, let json) = item.body {
            input = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] ?? [:]
        }
        func text(_ key: String) -> String? { (input[key] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        kind = text("subagent_type")
        model = text("model")
        title = text("description") ?? kind ?? "Sub-agent"
        prompt = input["prompt"] as? String ?? ""
        startedAt = times["\(item.turnId)/\(item.id)"]
        let requested = input["run_in_background"] as? Bool ?? false
        if let last = items.last(where: {
            guard $0.turnId == item.turnId, $0.parentCallId == item.parentCallId,
                  case .toolResult(let callId, _, _) = $0.body else { return false }
            return callId == item.id
        }), case .toolResult(_, let output, let isError) = last.body {
            let streamingResult = streaming.contains(last)
            launched = !isError && !streamingResult && (requested || Self.isLaunch(output))
            background = requested || launched
            if launched, let done = Self.completion(of: item, after: last, in: items),
               case .toolResult(_, let output, let isError) = done.body {
                result = output
                endedAt = times["\(done.turnId)/\(done.id)"]
                outcome = streaming.contains(done) ? .running : isError ? .failed : .ok
            } else {
                result = launched ? nil : output
                endedAt = launched ? nil : times["\(last.turnId)/\(last.id)"]
                // Without its completion, a launch is known to run only while the session does.
                outcome = streamingResult ? .running : isError ? .failed
                    : launched ? (working || runningTurn == item.turnId ? .running : .unknown) : .ok
            }
        } else {
            launched = false
            background = requested
            result = nil
            endedAt = nil
            outcome = runningTurn == item.turnId ? .running : .unknown
        }
    }

    /// A background agent's real result: the provider reports it after the launch, usually in
    /// a turn of its own (Claude starts one for it), as a later result for the same call. The
    /// search stops where a later call reuses the id, as a restarted CLI's may.
    static func completion(of call: Item, after launch: Item, in items: [Item]) -> Item? {
        guard let start = items.firstIndex(of: launch) else { return nil }
        for later in items[items.index(after: start)...] {
            if later.id == call.id { return nil }
            if later.parentCallId == call.parentCallId,
               case .toolResult(let callId, _, _) = later.body, callId == call.id {
                return later
            }
        }
        return nil
    }

    static func find(_ id: ID, in model: SessionModel) -> NativeAgent? {
        let items = model.log.compactMap { entry -> Item? in
            if case .item(let item) = entry { item } else { nil }
        } + model.streaming
        guard let item = items.first(where: { $0.id == id.callId && $0.turnId == id.turnId }) else { return nil }
        return NativeAgent(item: item, items: items, runningTurn: model.turn, working: model.status == .running,
                           streaming: model.streaming, times: model.itemTimes)
    }

    /// The agents a list shows under their session, so it is plain what runs: while the session
    /// works, its own agents (not theirs) that run and those that finished since its last turn
    /// started; none once it is idle, when every one has finished or stopped.
    static func listed(in model: SessionModel) -> [NativeAgent] {
        guard model.status == .running || model.turn != nil else { return [] }
        let items = model.log.compactMap { entry -> Item? in
            if case .item(let item) = entry { item } else { nil }
        } + model.streaming
        let since = model.turnStartedAt ?? .distantFuture
        return items.compactMap { item -> NativeAgent? in
            guard item.parentCallId == nil, case .toolCall(let name, _) = item.body, isAgent(name) else { return nil }
            let agent = NativeAgent(item: item, items: items, runningTurn: model.turn,
                                    working: model.status == .running, streaming: model.streaming, times: model.itemTimes)
            let finished = agent.outcome != .unknown && (agent.endedAt ?? .distantPast) >= since
            return agent.outcome == .running || finished ? agent : nil
        }
    }

    static func summary(_ agents: [NativeAgent]) -> String {
        let states: [(ToolCall.Outcome, String)] = [(.running, "working"), (.failed, "failed"), (.ok, "completed"), (.unknown, "stopped")]
        var parts = states.compactMap { outcome, label in
            let count = agents.filter { $0.outcome == outcome && !(outcome == .unknown && $0.launched) }.count
            return count > 0 ? "\(count) \(label)" : nil
        }
        let backgroundCount = agents.filter { $0.outcome == .unknown && $0.launched }.count
        if backgroundCount > 0 { parts.append("\(backgroundCount) background") }
        return parts.joined(separator: ", ")
    }

    var status: String {
        switch outcome {
        case .running: launched ? "Running in the background" : "Working"
        case .ok: "Completed"
        case .failed: "Failed"
        case .unknown: launched ? "In the background" : "Stopped without a result"
        }
    }

    /// The status as a badge: a word and its colour.
    var badge: (text: String, color: Color) {
        switch outcome {
        case .running: (launched ? "Background" : "Running", Theme.running)
        case .ok: ("Completed", Theme.success)
        case .failed: ("Failed", Theme.failure)
        case .unknown: (launched ? "Background" : "Stopped", Theme.secondary)
        }
    }

    /// How long the agent ran: to its result, or until now while it runs.
    func duration(at now: Date) -> TimeInterval? {
        guard let startedAt, let end = endedAt ?? (outcome == .running ? now : nil) else { return nil }
        return max(0, end.timeIntervalSince(startedAt))
    }
}

struct NativeAgentGroup: View {
    let agents: [NativeAgent]
    let fleet: Fleet
    let key: SessionKey
    @State private var expanded = true

    private var working: Int { agents.filter { $0.outcome == .running }.count }
    private var failed: Int { agents.filter { $0.outcome == .failed }.count }
    private var summary: String { NativeAgent.summary(agents) }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button { withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() } } label: {
                HStack(spacing: 12) {
                    HStack(spacing: -8) {
                        ForEach(agents.prefix(3)) { _ in
                            ProviderMark(provider: fleet.sessions[key]?.provider ?? "claude", size: 18)
                                .padding(7).background(Theme.surface, in: Circle())
                                .overlay(Circle().strokeBorder(Theme.stroke))
                        }
                    }
                    VStack(alignment: .leading, spacing: 3) {
                        Text("\(agents.count) \(agents.count == 1 ? "subagent" : "subagents")")
                            .font(.callout.weight(.semibold)).foregroundStyle(Theme.text)
                        Text(summary).font(.caption)
                            .foregroundStyle(working > 0 ? Theme.running : failed > 0 ? Theme.failure : Theme.secondary)
                    }
                    Spacer()
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption).foregroundStyle(Theme.tertiary)
                }
                .padding(.vertical, 6).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(agents.count) subagents, \(summary). \(expanded ? "Collapse" : "Expand")")
            if expanded {
                VStack(spacing: 0) {
                    ForEach(agents) { agent in NativeAgentCard(agent: agent, fleet: fleet, key: key) }
                }
                .padding(.vertical, 4)
                .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Theme.stroke))
            }
        }
    }
}

struct NativeAgentCard: View {
    let agent: NativeAgent
    let fleet: Fleet
    let key: SessionKey
    @State private var showingChat = false

    var body: some View {
        Button { showingChat = true } label: {
            HStack(spacing: 12) {
                ProviderMark(provider: fleet.sessions[key]?.provider ?? "claude", size: 20)
                    .padding(8).background(Theme.surface, in: Circle())
                    .overlay(alignment: .bottomTrailing) {
                        Circle().fill(agent.outcome == .running ? Theme.running : agent.outcome == .failed ? Theme.failure : Theme.idle)
                            .frame(width: 9, height: 9).overlay(Circle().strokeBorder(Theme.background, lineWidth: 2))
                    }
                VStack(alignment: .leading, spacing: 4) {
                    Text(agent.title).font(.callout.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(2)
                    Text(agent.status).font(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer(minLength: 8)
                if agent.outcome == .running { ProgressView().controlSize(.small) }
                if agent.outcome == .failed { Image(systemName: "exclamationmark.circle").foregroundStyle(Theme.failure) }
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Open sub-agent: \(agent.title), \(agent.status)")
        .sheet(isPresented: $showingChat) {
            NativeAgentChat(reference: agent.id, fleet: fleet, key: key)
        }
    }
}

/// A provider agent under its session in a list: its task and how it is doing; a tap opens
/// its sheet.
struct NativeAgentRow: View {
    let agent: NativeAgent
    let fleet: Fleet
    let key: SessionKey
    /// Its session's depth in the task tree.
    var depth = 0
    @State private var showingChat = false
    @State private var hovering = false

    var body: some View {
        Button { showingChat = true } label: {
            HStack(spacing: 8) {
                TreeLine().frame(width: CGFloat(depth + 1) * 14)
                ProviderMark(provider: fleet.sessions[key]?.provider ?? "claude", size: 11)
                    .frame(width: 20, height: 20)
                    .background(Theme.raised, in: Circle())
                    .overlay(Circle().strokeBorder(Theme.stroke))
                Text(agent.title).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                Spacer(minLength: 6)
                if agent.outcome == .running { ProgressView().controlSize(.mini) }
                Text(agent.badge.text).font(.caption.weight(.semibold)).foregroundStyle(agent.badge.color).fixedSize()
            }
            .padding(.vertical, 5)
            .padding(.horizontal, 8)
            .background(hovering ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(agent.prompt)
        .accessibilityLabel("Open sub-agent: \(agent.title), \(agent.status)")
        .sheet(isPresented: $showingChat) {
            NativeAgentChat(reference: agent.id, fleet: fleet, key: key)
        }
    }
}

/// The sheet a sub-agent opens in: who it is and how it did, its task, its own transcript,
/// and what it returned.
struct NativeAgentChat: View {
    let reference: NativeAgent.ID
    let fleet: Fleet
    let key: SessionKey
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Group {
            if let model = fleet.sessions[key], let agent = NativeAgent.find(reference, in: model) {
                NativeAgentDetail(agent: agent, model: model, fleet: fleet, key: key) { dismiss() }
            } else {
                VStack(spacing: 14) {
                    Text("This sub-agent is no longer available.").foregroundStyle(Theme.secondary)
                    Button("Close") { dismiss() }.keyboardShortcut(.cancelAction)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Theme.background)
            }
        }
        #if os(macOS)
        .frame(minWidth: 600, idealWidth: 760, minHeight: 480, idealHeight: 700)
        #endif
        .preferredColorScheme(.dark)
    }
}

struct NativeAgentDetail: View {
    let agent: NativeAgent
    let model: SessionModel
    let fleet: Fleet
    let key: SessionKey
    let close: () -> Void

    var body: some View {
        let blocks = Transcript.blocks(model, parent: agent.id)
        VStack(spacing: 0) {
            header
            Rectangle().fill(Theme.stroke).frame(height: 1)
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    if !agent.prompt.isEmpty {
                        section("Task") { AgentTask(text: agent.prompt) }
                    }
                    section("Transcript", count: blocks.isEmpty ? nil : blocks.count) {
                        if blocks.isEmpty {
                            Text(agent.outcome == .running && !agent.launched
                                 ? "Waiting for the agent’s first message…"
                                 : "No messages from this agent were recorded.")
                                .font(.callout).foregroundStyle(Theme.tertiary)
                        } else {
                            VStack(alignment: .leading, spacing: 16) {
                                ForEach(blocks) { block in
                                    TranscriptBlockView(block: block, fleet: fleet, key: key, open: nil)
                                }
                            }
                        }
                    }
                    section(agent.outcome == .failed ? "Error" : "Result") { result }
                    Text("The parent conversation manages this agent. Answer it or steer it from there.")
                        .font(.footnote).foregroundStyle(Theme.tertiary)
                }
                .frame(maxWidth: 760, alignment: .leading)
                .frame(maxWidth: .infinity)
                .padding(20)
            }
        }
        .background(Theme.background)
    }

    private var parentTitle: String { model.title ?? "Conversation" }

    /// The way back, then the agent: its provider and state, kind and model, title, badge and run time.
    private var header: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 6) {
                Button(action: close) {
                    HStack(spacing: 4) {
                        Image(systemName: "chevron.left")
                        Text(parentTitle).lineLimit(1).truncationMode(.middle)
                    }
                    .foregroundStyle(Theme.text)
                    .padding(.horizontal, 8)
                    .frame(height: 24)
                    .background(Theme.raised, in: .rect(cornerRadius: 6))
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.cancelAction)
                .help("Back to the conversation (Esc)")
                .accessibilityLabel("Back to \(parentTitle)")
                Image(systemName: "chevron.right").font(.caption2.weight(.bold)).foregroundStyle(Theme.tertiary)
                Text("Sub-agent").foregroundStyle(Theme.tertiary).lineLimit(1)
                Spacer(minLength: 8)
            }
            .font(.caption.weight(.semibold))
            HStack(alignment: .center, spacing: 14) {
                ProviderMark(provider: model.provider ?? "claude", size: 22)
                    .padding(10)
                    .background(Theme.surface, in: Circle())
                    .overlay(Circle().strokeBorder(Theme.stroke))
                    .overlay(alignment: .bottomTrailing) {
                        Circle().fill(agent.badge.color).frame(width: 11, height: 11)
                            .overlay(Circle().strokeBorder(Theme.background, lineWidth: 2.5))
                    }
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(agent.kind ?? "Sub-agent").foregroundStyle(Theme.secondary)
                        if let name = agent.model {
                            Text("·").foregroundStyle(Theme.tertiary)
                            Text(name).foregroundStyle(Theme.tertiary)
                        }
                    }
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                    Text(agent.title)
                        .font(.title3.weight(.bold))
                        .foregroundStyle(Theme.text)
                        .lineLimit(2)
                        .textSelection(.enabled)
                }
                Spacer(minLength: 12)
                VStack(alignment: .trailing, spacing: 6) {
                    AgentBadge(agent: agent)
                    AgentRunTime(agent: agent)
                }
            }
        }
        .padding(.horizontal, 20)
        .padding(.top, 16)
        .padding(.bottom, 18)
    }

    /// What the agent returned, as Markdown; else where its result stands.
    @ViewBuilder private var result: some View {
        if let result = agent.result, !result.isEmpty {
            MarkdownText(text: result)
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner)
                    .strokeBorder(agent.outcome == .failed ? Theme.failure.opacity(0.5) : Theme.stroke))
        } else {
            let (symbol, text): (String, String) = switch (agent.outcome, agent.launched) {
            case (.running, false): ("", "Working. The result appears here when the agent finishes.")
            case (.running, true): ("moon.stars", "Running in the background. The parent conversation gets the result when the agent finishes.")
            case (.unknown, true): ("tray", "Launched in the background. herder has not recorded its result; the parent conversation gets it when the agent finishes.")
            case (.ok, _): ("checkmark.circle", "Completed without a result.")
            case (.failed, _): ("exclamationmark.circle", "Failed without saying why.")
            case (.unknown, false): ("stop.circle", "Stopped without a result.")
            }
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                if symbol.isEmpty { ProgressView().controlSize(.small) } else { Image(systemName: symbol) }
                Text(text).fixedSize(horizontal: false, vertical: true)
            }
            .font(.callout)
            .foregroundStyle(Theme.secondary)
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke, style: StrokeStyle(lineWidth: 1, dash: [4, 3])))
        }
    }

    private func section(_ title: String, count: Int? = nil, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeading(title: title, count: count)
            content()
        }
    }
}

/// The task the parent gave, folded when long.
private struct AgentTask: View {
    let text: String
    @State private var expanded = false

    private var long: Bool { text.count > 420 || text.split(separator: "\n", omittingEmptySubsequences: false).count > 6 }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(text)
                .font(.callout)
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .lineLimit(long && !expanded ? 6 : nil)
                .frame(maxWidth: .infinity, alignment: .leading)
            if long {
                Button { withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() } } label: {
                    Label(expanded ? "Show less" : "Show more", systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.secondary)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(14)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// The agent's state as a small coloured badge.
private struct AgentBadge: View {
    let agent: NativeAgent

    var body: some View {
        let (text, color) = agent.badge
        HStack(spacing: 5) {
            if agent.outcome == .running {
                Circle().fill(color).frame(width: 6, height: 6)
            }
            Text(text)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(color)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(color.opacity(0.14), in: .capsule)
        .fixedSize()
        .help(agent.status)
        .accessibilityLabel(agent.status)
    }
}

/// How long the agent ran, ticking while it runs.
private struct AgentRunTime: View {
    let agent: NativeAgent

    var body: some View {
        Group {
            if agent.outcome == .running, agent.startedAt != nil {
                TimelineView(.periodic(from: .now, by: 1)) { context in text(at: context.date) }
            } else if agent.duration(at: .now) != nil {
                text(at: .now)
            }
        }
        .font(.caption.monospacedDigit())
        .foregroundStyle(Theme.tertiary)
    }

    private func text(at now: Date) -> some View {
        Label(Duration.seconds((agent.duration(at: now) ?? 0).rounded())
            .formatted(.units(allowed: [.hours, .minutes, .seconds], width: .narrow, maximumUnitCount: 2)),
              systemImage: "clock")
            .labelStyle(.titleAndIcon)
            .help("Time the agent ran")
    }
}
