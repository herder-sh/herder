import Foundation
import Herder

extension HerderError {
    /// The error as a sentence for the UI. The generated `errorDescription` is a debug dump.
    public var message: String {
        switch self {
        case .InvalidLink(let detail), .Local(let detail): detail
        case .Unreachable(let detail): "No address answered: \(detail)"
        case .UnknownMachine(let hostId): "No paired machine \(hostId)."
        case .Rejected(let info): needsNewerHerder ? Self.newerHerder : info.message
        case .Closed: "herder stopped."
        case .Demo: "This is a demo; nothing is sent."
        }
    }

    /// Whether the machine refused a command it does not know: its herder is older than this
    /// app, and says so with the serde error that lists every command it does know.
    public var needsNewerHerder: Bool {
        guard case .Rejected(let info) = self else { return false }
        return info.code == .badRequest && info.message.contains("unknown variant `")
    }

    static let newerHerder = "This needs a newer herder on that machine."
}

/// Any error as a sentence for the UI.
public func describe(_ error: any Error) -> String {
    (error as? HerderError)?.message ?? error.localizedDescription
}

/// An error's "context: cause: root cause" chain, one link a line.
func chainLines(_ message: String) -> String {
    message.components(separatedBy: ": ")
        .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
        .filter { !$0.isEmpty }
        .joined(separator: "\n")
}
