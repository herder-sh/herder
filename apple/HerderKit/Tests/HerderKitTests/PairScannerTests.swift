@testable import HerderKit
import Testing

struct PairScannerTests {
    let link = "herder://pair?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=ABCD2345"

    @Test func findsTheLinkAlone() {
        #expect(pairingLink(in: link) == link)
        #expect(pairingLink(in: "  \(link)\n") == link)
    }

    @Test func findsTheLinkInHerderPairOutput() {
        let output = """
            Pair a device as alice (owner): scan the code, or enter these in the app.

              address      127.0.0.1:7420
              code         ABCD2345

            \(link)

            In a terminal, run `herder connect '<link>'` with it, or paste it into herder.
            """
        #expect(pairingLink(in: output) == link)
        #expect(pairingLink(in: "herder connect '\(link)'") == link)
    }

    @Test func findsALinkOfSeveralMachines() {
        let shared = link + "&host=10.0.0.9%3A7447&fp=" + String(repeating: "cd", count: 32) + "&code=MNPQR-STVWX"
        #expect(pairingLink(in: shared) == shared)
        #expect(pairingLink(in: "Pair with \(shared) now") == shared)
    }

    @Test func rejectsWhatIsNotAPairingLink() {
        #expect(pairingLink(in: "") == nil)
        #expect(pairingLink(in: "https://herder.sh") == nil)
        #expect(pairingLink(in: "herder://pair?host=127.0.0.1%3A7420") == nil)
    }
}
