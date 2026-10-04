import Foundation
import Herder

extension HerderError {
    /// The error as a sentence for the UI. The generated `errorDescription` is a debug dump.
    public var message: String {
        switch self {
        case .InvalidLink(let detail), .Local(let detail): detail
        case .Pairing(let detail): "Pairing failed: \(detail)"
        case .Unreachable(let detail): "No address answered: \(detail)"
        case .UnknownMachine(let hostId): "No paired machine \(hostId)."
        case .Rejected(let info): info.message
        case .Closed: "herder stopped."
        }
    }
}

/// Any error as a sentence for the UI.
public func describe(_ error: any Error) -> String {
    (error as? HerderError)?.message ?? error.localizedDescription
}
