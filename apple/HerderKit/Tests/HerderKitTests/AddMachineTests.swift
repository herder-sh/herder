import Herder
@testable import HerderKit
import Testing

struct AddMachineTests {
    let fingerprint = String(repeating: "ab", count: 32)
    var own: String { "herder://own?host=127.0.0.1%3A7448&fp=\(fingerprint)&code=K7Q-4PD&name=draugelis" }

    @Test func takesTheOwnLinkHerderClaimPrints() throws {
        let output = """
            Scan the code with the herder app, or open this link on a device signed in to it:

            \(own)

            Check that the app shows K7Q · 4PD.
            """
        let request = try #require(approvalRequest(in: output))
        #expect(request.kind == .machine)
        #expect(request.name == "draugelis" && request.code == "K7Q-4PD" && request.fingerprint == fingerprint)
        #expect(request.hosts == ["127.0.0.1:7448"])
    }

    @Test func takesALoginLinkToApprove() throws {
        let request = try #require(approvalRequest(in: own.replacingOccurrences(of: "herder://own", with: "herder://login")))
        #expect(request.kind == .login)
    }

    @Test func refusesPairingLinksAndCodes() {
        #expect(approvalRequest(in: "herder://pair?host=127.0.0.1%3A7448&fp=\(fingerprint)&code=ABCDE-FGHJK") == nil)
        #expect(approvalRequest(in: "address 127.0.0.1:7448\nfingerprint \(fingerprint)\ncode ABCDE-FGHJK") == nil)
        #expect(approvalRequest(in: "ABCDE-FGHJK") == nil)
        #expect(approvalRequest(in: "") == nil)
        #expect(PairKind.command == "herder claim")
        #expect(PairKind.allCases == [.machine, .vault])
    }
}
