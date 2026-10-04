import Herder
import SwiftUI

/// Sender identity is durable daemon metadata, never inferred from message text.
struct AgentMessageSource: View {
    let message: AgentMessage
    let fleet: Fleet
    let hostId: HostId
    let open: ((SessionKey) -> Void)?

    private var source: SessionKey { SessionKey(hostId: hostId, sessionId: message.senderSessionId) }

    private var senderAvailable: Bool {
        fleet.machines.first { $0.hostId == hostId }?.sessions.contains {
            $0.sessionId == message.senderSessionId
        } == true
    }

    var body: some View {
        VStack(alignment: .trailing, spacing: 3) {
            Label("Sent by another agent", systemImage: "bubble.left.and.bubble.right")
                .font(.caption).foregroundStyle(Theme.secondary)
            // Replayed or forked history may name a session unavailable on this host.
            if !senderAvailable {
                senderLabel
            } else if let open {
                Button { open(source) } label: { senderLabel }.buttonStyle(.plain)
            } else {
                NavigationLink(value: source) { senderLabel }.buttonStyle(.plain)
            }
        }
        .help("Sender session: \(message.senderSessionId)")
    }

    private var senderLabel: some View {
        HStack(spacing: 4) {
            Text(fleet.senderTitle(message, on: hostId))
                .lineLimit(1).truncationMode(.middle)
            if senderAvailable { Image(systemName: "arrow.up.right") }
        }
        .font(.caption).foregroundStyle(Theme.tertiary)
        .accessibilityLabel("\(senderAvailable ? "Open sender session" : "Sender session") \(message.senderSessionId)")
    }
}

extension Fleet {
    /// The title of the session another agent's message came from, or the end of its id.
    func senderTitle(_ message: AgentMessage, on hostId: HostId) -> String {
        sessions[SessionKey(hostId: hostId, sessionId: message.senderSessionId)]?.title
            ?? "Session …\(message.senderSessionId.suffix(6))"
    }
}
