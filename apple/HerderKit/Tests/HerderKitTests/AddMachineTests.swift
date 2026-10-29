import Herder
@testable import HerderKit
import Testing

struct AddMachineTests {
    let fingerprint = String(repeating: "ab", count: 32)
    var own: String { "herder://own?host=127.0.0.1%3A7448&fp=\(fingerprint)&code=K7Q-4PD&name=draugelis" }
    var shared: String { "herder://pair?host=127.0.0.1%3A7448&fp=\(fingerprint)&code=ABCDE-FGHJK" }

    @Test func takesTheOwnLinkHerderClaimPrints() throws {
        let output = """
            Scan the code with the herder app, or open this link on a device signed in to it:

            \(own)

            Check that the app shows K7Q · 4PD.
            """
        guard case .own(let request)? = PairInput(output) else {
            Issue.record("expected the own link")
            return
        }
        #expect(request.name == "draugelis" && request.code == "K7Q-4PD" && request.fingerprint == fingerprint)
        #expect(request.hosts == ["127.0.0.1:7448"])
    }

    @Test func takesALinkAnotherDeviceShared() throws {
        guard case .shared(let link, let machines)? = PairInput(shared) else {
            Issue.record("expected the shared link")
            return
        }
        #expect(link == shared && machines.count == 1)
    }

    @Test func refusesWhatHerderPairUsedToPrintAndLoginLinks() {
        // A terminal's login link is approved from its own sheet, not added to the fleet.
        #expect(PairInput(own.replacingOccurrences(of: "herder://own", with: "herder://login")) == nil)
        #expect(PairInput("address 127.0.0.1:7448\nfingerprint \(fingerprint)\ncode ABCDE-FGHJK") == nil)
        #expect(PairInput("ABCDE-FGHJK") == nil)
        #expect(PairInput("") == nil)
        #expect(PairKind.command == "herder claim")
    }
}
