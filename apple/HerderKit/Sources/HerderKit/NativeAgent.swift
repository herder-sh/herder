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
    let prompt: String
    let outcome: ToolCall.Outcome
    let result: String?

    static func isAgent(_ name: String) -> Bool {
        ["agent", "task"].contains(name.lowercased())
    }

    init(item: Item, items: [Item], runningTurn: TurnId?, streaming: [Item] = []) {
        id = ID(turnId: item.turnId, callId: item.id)
        var input: [String: Any] = [:]
        if case .toolCall(_, let json) = item.body {
            input = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] ?? [:]
        }
        title = (input["description"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            ?? (input["subagent_type"] as? String) ?? "Sub-agent"
        prompt = input["prompt"] as? String ?? ""
        if let last = items.last(where: {
            guard $0.turnId == item.turnId, $0.parentCallId == item.parentCallId,
                  case .toolResult(let callId, _, _) = $0.body else { return false }
            return callId == item.id
        }), case .toolResult(_, let output, let isError) = last.body {
            result = output
            outcome = streaming.contains(last) ? .running : isError ? .failed : .ok
        } else {
            result = nil
            outcome = runningTurn == item.turnId ? .running : .unknown
        }
    }

    static func find(_ id: ID, in model: SessionModel) -> NativeAgent? {
        let items = model.log.compactMap { entry -> Item? in
            if case .item(let item) = entry { item } else { nil }
        } + model.streaming
        guard let item = items.first(where: { $0.id == id.callId && $0.turnId == id.turnId }) else { return nil }
        return NativeAgent(item: item, items: items, runningTurn: model.turn, streaming: model.streaming)
    }

    static func summary(_ agents: [NativeAgent]) -> String {
        let states: [(ToolCall.Outcome, String)] = [(.running, "working"), (.failed, "failed"), (.ok, "completed"), (.unknown, "stopped")]
        return states.compactMap { outcome, label in
            let count = agents.filter { $0.outcome == outcome }.count
            return count > 0 ? "\(count) \(label)" : nil
        }.joined(separator: ", ")
    }

    var status: String {
        switch outcome {
        case .running: "Working"
        case .ok: "Completed"
        case .failed: "Failed"
        case .unknown: "Stopped without a result"
        }
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
        .accessibilityLabel("Open sub-chat: \(agent.title), \(agent.status)")
        .sheet(isPresented: $showingChat) {
            NativeAgentChat(reference: agent.id, fleet: fleet, key: key)
        }
    }
}

private struct NativeAgentChat: View {
    let reference: NativeAgent.ID
    let fleet: Fleet
    let key: SessionKey
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if let model = fleet.sessions[key], let agent = NativeAgent.find(reference, in: model) {
                        Text(agent.title).font(.title2.weight(.semibold)).foregroundStyle(Theme.text)
                        Text(agent.status).font(.caption).foregroundStyle(agent.outcome == .failed ? Theme.failure : Theme.secondary)
                        if !agent.prompt.isEmpty {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("Task").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                                Text(agent.prompt).textSelection(.enabled).foregroundStyle(Theme.text)
                            }
                            .padding(14).frame(maxWidth: .infinity, alignment: .leading)
                            .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
                        }
                        let blocks = Transcript.blocks(model, parent: reference)
                        ForEach(blocks) { block in
                            TranscriptBlockView(block: block, fleet: fleet, key: key, open: nil)
                        }
                        if let result = agent.result, !result.isEmpty {
                            Divider()
                            Text("Result").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                            MarkdownText(text: result)
                        } else if blocks.isEmpty {
                            Text(agent.outcome == .running
                                 ? "Waiting for the sub-agent’s first message…"
                                 : "No nested messages were recorded for this agent.")
                                .foregroundStyle(Theme.secondary)
                        }
                        Text("This agent is managed by the parent conversation. Return there to answer questions or give instructions.")
                            .font(.footnote).foregroundStyle(Theme.tertiary)
                    } else {
                        Text("This sub-chat is no longer available.").foregroundStyle(Theme.secondary)
                    }
                }
                .padding(24).frame(maxWidth: .infinity, alignment: .leading)
            }
            .background(Theme.background)
            .navigationTitle("Sub-chat")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Back to parent") { dismiss() }
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 580, idealWidth: 720, minHeight: 440, idealHeight: 650)
        #endif
    }
}
