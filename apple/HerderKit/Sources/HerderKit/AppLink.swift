import Foundation
import Herder

/// What a `herder://` link opens: a session, as the bots' "Open in herder" button links it,
/// Add to Fleet with the link another device shared, or the approval a new machine's or a
/// terminal's link asks for.
enum AppLink: Equatable {
    /// `herder://session?host=…&session=…`.
    case session(SessionKey)
    /// `herder://pair?host=…&fp=…&code=…`, of one machine or several: the link itself.
    case pair(String)
    /// `herder://own?…` of `herder install`, or `herder://login?…` of `herder login`.
    case approve(ApprovalRequest)

    /// Nil for a link of another scheme or kind, or one missing what it needs.
    init?(url: URL) {
        guard url.scheme?.lowercased() == "herder",
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        switch url.host()?.lowercased() {
        case "session":
            let items = components.queryItems ?? []
            func value(_ name: String) -> String? {
                items.first { $0.name == name }?.value.flatMap { $0.isEmpty ? nil : $0 }
            }
            guard let host = value("host"), let session = value("session") else { return nil }
            self = .session(SessionKey(hostId: host, sessionId: session))
        case "pair":
            let link = url.absoluteString
            guard (try? parsePairingLink(link: link)) != nil else { return nil }
            self = .pair(link)
        case "own", "login":
            guard let request = ApprovalRequest(url: url) else { return nil }
            self = .approve(request)
        default:
            return nil
        }
    }
}

/// Where a session link leads, given the paired machines as they stand.
enum SessionLinkTarget: Equatable {
    case open
    /// No paired machine is the link's host.
    case unpaired
    /// Its machine has not connected yet, so whether it lists the session is unknown.
    case connecting
    case notFound
    case archived

    init(_ key: SessionKey, machines: [Machine]) {
        guard let machine = machines.first(where: { $0.hostId == key.hostId }) else {
            self = .unpaired
            return
        }
        guard let head = machine.sessions.first(where: { $0.sessionId == key.sessionId }) else {
            self = machine.connection == .connected ? .notFound : .connecting
            return
        }
        self = head.status == .archived ? .archived : .open
    }
}

extension SessionLinkTarget {
    /// The alert's title, when the session does not open.
    var title: String {
        switch self {
        case .open: ""
        case .unpaired: "Machine not paired"
        case .connecting: "Machine not connected"
        case .notFound: "Session not found"
        case .archived: "Session archived"
        }
    }

    var message: String {
        switch self {
        case .open: ""
        case .unpaired: "This session is on a machine this device isn't paired with."
        case .connecting: "This session's machine hasn't connected, so herder can't open the session."
        case .notFound: "Its machine has no such session; it may have been deleted."
        case .archived: "This session is archived."
        }
    }
}
