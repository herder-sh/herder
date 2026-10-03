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
            Text(fleet.sessions[source]?.title ?? "Session …\(message.senderSessionId.suffix(6))")
                .lineLimit(1).truncationMode(.middle)
            Image(systemName: "arrow.up.right")
        }
        .font(.caption).foregroundStyle(Theme.tertiary)
        .accessibilityLabel("Open sender session \(message.senderSessionId)")
    }
}
