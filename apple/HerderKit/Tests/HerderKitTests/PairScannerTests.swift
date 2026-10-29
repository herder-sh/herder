@testable import HerderKit
import Testing

struct PairScannerTests {
    let own = "herder://own?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=K7Q-4PD"
    let link = "herder://pair?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=ABCD2345"

    @Test func findsTheLinkAlone() {
        #expect(fleetLink(in: own) == own)
        #expect(fleetLink(in: "  \(link)\n") == link)
    }

    @Test func findsTheLinkInHerderClaimOutput() {
        let output = """
            Scan the code with the herder app, or open this link:

            \(own)

            Check that the app shows K7Q · 4PD.
            """
        #expect(fleetLink(in: output) == own)
        #expect(fleetLink(in: "open '\(own)'") == own)
    }

    @Test func findsALinkOfSeveralMachines() {
        let shared = link + "&host=10.0.0.9%3A7447&fp=" + String(repeating: "cd", count: 32) + "&code=MNPQR-STVWX"
        #expect(fleetLink(in: shared) == shared)
        #expect(fleetLink(in: "Pair with \(shared) now") == shared)
    }

    @Test func rejectsWhatIsNotAFleetLink() {
        #expect(fleetLink(in: "") == nil)
        #expect(fleetLink(in: "https://herder.sh") == nil)
        #expect(fleetLink(in: "herder://pair?host=127.0.0.1%3A7420") == nil)
        #expect(fleetLink(in: own.replacingOccurrences(of: "own", with: "login")) == nil)
    }
}
