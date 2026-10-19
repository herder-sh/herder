import Herder
@testable import HerderKit
import Testing

@MainActor
struct AwsSignInTests {
    private let device = AwsDeviceCode(verificationUrl: "https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH",
                                       userCode: "ABCD-EFGH")

    private func status(_ state: TerminalConnection.State, refusal: ErrorCode? = nil,
                        terminals: [Terminal] = []) -> AwsSignInStatus {
        AwsSignInStatus(state: state, refusal: refusal, terminalId: "t1", terminals: terminals)
    }

    @Test func aRowWaitsWithTheCodeOnceTheTerminalListCarriesIt() {
        #expect(status(.connecting) == .starting)
        let printing = Terminal(terminalId: "t1", purpose: .awsLogin(profile: "trip1-prod", device: nil))
        #expect(status(.attached, terminals: [printing]) == .starting)
        let prompted = Terminal(terminalId: "t1", purpose: .awsLogin(profile: "trip1-prod", device: device))
        let other = Terminal(terminalId: "t2", purpose: .awsLogin(profile: "trip1-prod", device: nil))
        #expect(status(.attached, terminals: [other, prompted]) == .waiting(device))
        #expect(!status(.attached, terminals: [prompted]).isFinal)
    }

    @Test func theExitCodeTellsSignedInFromFailed() {
        #expect(status(.exited(0)) == .signedIn)
        #expect(status(.exited(1)) == .failed(1))
        #expect(status(.exited(1)).label == "Failed (exit 1)")
        #expect(status(.exited(nil)) == .failed(nil))
        #expect(status(.exited(0)).isFinal && status(.exited(1)).isFinal)
    }

    @Test func refusalsNameTheirCause() {
        #expect(status(.failed("aws is not installed"), refusal: .unsupported) == .notInstalled)
        #expect(status(.failed("no such profile"), refusal: .badRequest) == .noSuchProfile)
        #expect(status(.failed("timed out")) == .refused("timed out"))
    }

    @Test func choicesJoinEveryMachinesProfilesIdentityCenterFirst() {
        let choices = AwsSignIn.choices([
            "a": [AwsProfile(name: "default", sso: false), AwsProfile(name: "trip1-prod", sso: true)],
            "b": [AwsProfile(name: "trip1-prod", sso: true), AwsProfile(name: "admin", sso: true),
                  AwsProfile(name: "default", sso: false)],
        ])
        #expect(choices.map(\.name) == ["admin", "trip1-prod", "default"])
        #expect(choices.map(\.sso) == [true, true, false])
    }
}
