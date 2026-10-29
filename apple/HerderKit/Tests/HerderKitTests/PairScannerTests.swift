@testable import HerderKit
import Testing

struct PairScannerTests {
    let own = "herder://own?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=K7Q-4PD"
    var login: String { own.replacingOccurrences(of: "herder://own", with: "herder://login") }

    @Test func findsTheLinkAlone() {
        #expect(approvalRequest(in: own)?.kind == .machine)
        #expect(approvalRequest(in: "  \(login)\n")?.kind == .login)
    }

    @Test func findsTheLinkInHerderClaimOutput() {
        let output = """
            Scan the code with the herder app, or open this link:

            \(own)

            Check that the app shows K7Q · 4PD.
            """
        #expect(approvalRequest(in: output)?.code == "K7Q-4PD")
        #expect(approvalRequest(in: "open '\(own)'")?.code == "K7Q-4PD")
    }

    @Test func rejectsWhatIsNotAnOwnOrLoginLink() {
        #expect(approvalRequest(in: "") == nil)
        #expect(approvalRequest(in: "https://herder.sh") == nil)
        #expect(approvalRequest(in: "herder://own?host=127.0.0.1%3A7420") == nil)
        let pair = "herder://pair?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=ABCD2345"
        #expect(approvalRequest(in: pair) == nil)
    }
}
