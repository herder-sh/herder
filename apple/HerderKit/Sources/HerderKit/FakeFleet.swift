import Foundation
import Herder

extension Fleet {
    /// Joins the fake daemons that UI tests and screenshot runs launch the app against, whose
    /// links are in `HERDER_FAKE_FLEET`, separated by whitespace. A link is skipped once this
    /// device has all its machines, since each code works once. `fake_daemon` still hands out
    /// codes, and no screen of the app takes them.
    func joinFakeFleet() async {
        guard let links = ProcessInfo.processInfo.environment["HERDER_FAKE_FLEET"] else { return }
        let known = Set(client.machines().map(\.fingerprint))
        for link in links.split(whereSeparator: \.isWhitespace).map(String.init) {
            guard let parsed = try? parsePairingLink(link: link),
                  parsed.machines.contains(where: { !known.contains($0.fingerprint) })
            else { continue }
            _ = try? await client.pair(link: link)
        }
    }
}
