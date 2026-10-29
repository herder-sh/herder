import CryptoKit
import Foundation
import Herder

extension Fleet {
    /// Signs in to the fake daemons that UI tests, screenshot and profile runs launch the app
    /// against: the machines of the `herder://fleet` links in `HERDER_FAKE_FLEET`, separated by
    /// whitespace, as the person whose recorded passkey `HERDER_FAKE_PASSKEY` holds, as
    /// `fake_daemon` prints both. A machine this device has already is skipped.
    func joinFakeFleet() async {
        let environment = ProcessInfo.processInfo.environment
        guard let links = environment["HERDER_FAKE_FLEET"],
              let passkey = environment["HERDER_FAKE_PASSKEY"].flatMap(RecordedPasskey.init)
        else { return }
        let known = Set(client.machines().map(\.fingerprint))
        for link in links.split(whereSeparator: \.isWhitespace).map(String.init) {
            guard let parsed = try? parseFleetLink(link: link) else { continue }
            for machine in parsed.machines where !known.contains(machine.fingerprint) {
                _ = try? await signIn(hosts: machine.hosts, fingerprint: machine.fingerprint, with: passkey)
            }
        }
    }
}

/// A recorded test passkey, as `fake_daemon` prints it: `<credential id>.<PKCS#8 key>.<user
/// handle>`, each base64url. It signs without Face ID, and only on the fake fleet.
struct RecordedPasskey: Authenticator {
    let credentialId: Data
    let key: P256.Signing.PrivateKey
    let userHandle: Data

    init?(_ exported: String) {
        let parts = exported.split(separator: ".").map(String.init)
        guard parts.count == 3,
              let credentialId = Data(base64URL: parts[0]),
              let der = Data(base64URL: parts[1]),
              let key = try? P256.Signing.PrivateKey(derRepresentation: der),
              let userHandle = Data(base64URL: parts[2])
        else { return nil }
        (self.credentialId, self.key, self.userHandle) = (credentialId, key, userHandle)
    }

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        throw HerderError.Local(detail: "a recorded passkey creates no other")
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        try SoftwareSigning.assertion(key: key, credentialId: credentialId, userHandle: userHandle, challenge: challenge)
    }
}
