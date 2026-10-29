import AuthenticationServices
import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct ActionRequestTests {
    nonisolated static func request(_ id: String, _ action: String = "Open a terminal", client: String = "herder-tui/0.17.0",
                                    expires: String = "2026-10-29T12:02:00Z") -> Herder.ApprovalRequest {
        Herder.ApprovalRequest(requestId: id, action: action, client: client, expiresAt: expires)
    }

    nonisolated static func machine(_ hostId: HostId, _ name: String, approvals: [Herder.ApprovalRequest]) -> Machine {
        var machine = HerderKitTests.machine(hostId, name: name, sessions: [])
        machine.approvals = approvals
        return machine
    }

    @Test func everyMachinesRequestsShowOldestFirstWithTheMachineTheyAsk() {
        let requests = Fleet.actionRequests([
            Self.machine("h1", "trash-can-01", approvals: [Self.request("r2", expires: "2026-10-29T12:03:00Z")]),
            Self.machine("h2", "draugelis", approvals: [Self.request("r1", "Store the secret TOKEN in deploy")]),
        ])
        #expect(requests.map(\.id) == ["h2/r1", "h1/r2"])
        #expect(requests.map(\.title) == ["Store the secret TOKEN in deploy on draugelis?", "Open a terminal on trash-can-01?"])
    }

    @Test func theRequestSaysWhichDeviceAsks() {
        let tui = ActionRequest(hostId: "h", machine: "m", request: Self.request("r"))
        #expect(tui.from == "From the TUI · herder 0.17.0")
        let cli = ActionRequest(hostId: "h", machine: "m", request: Self.request("r", client: "herder-cli/0.17.0"))
        #expect(cli.from == "From the herder CLI · herder 0.17.0")
        let other = ActionRequest(hostId: "h", machine: "m", request: Self.request("r", client: "lab"))
        #expect(other.from == "From lab")
    }

    @Test func approvingSignsAndRefusingSaysNo() async {
        var settled: [Bool] = []
        let request = ActionRequest(hostId: "h", machine: "m", request: Self.request("r"))
        let approval = ActionApproval(request) { settled.append($0) }
        await approval.approve()
        #expect(approval.state == .settled)
        let refusal = ActionApproval(request) { settled.append($0) }
        await refusal.refuse()
        #expect(settled == [true, false])
    }

    @Test func aCancelledFaceIDLeavesTheRequestToApproveAgain() async {
        let request = ActionRequest(hostId: "h", machine: "m", request: Self.request("r"))
        let approval = ActionApproval(request) { _ in throw HerderError.Local(detail: "Face ID was cancelled") }
        await approval.approve()
        #expect(approval.state == .failed("Face ID was cancelled"))
    }
}

@MainActor
struct FaceIDTests {
    /// Signs every challenge it is given, unless told to fail as Face ID would.
    @MainActor final class Passkey: Authenticator {
        var failure: Error?
        private(set) var signed: [Data] = []

        func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
            throw HerderError.Local(detail: "not in these tests")
        }

        func assert(challenge: Data) async throws -> PasskeyAssertion {
            if let failure { throw failure }
            signed.append(challenge)
            return PasskeyAssertion(id: "AQ", rawId: "AQ", response: AssertionResponse(
                clientDataJson: "", authenticatorData: "", signature: "", userHandle: nil), kind: "public-key")
        }
    }

    private let challenge = PasskeyChallenge(rpId: "herder.sh", challenge: "CQk", expiresAt: "2026-10-29T12:01:00Z")

    @Test func theClientCoresAssertionIsTheDevicesPasskeySigningTheMachinesChallenge() async throws {
        let passkey = Passkey()
        let assertion = try await FaceID { passkey }.assert(hostId: "h", challenge: challenge)
        #expect(assertion.id == "AQ")
        #expect(passkey.signed == [Data([9, 9])])
    }

    @Test func closingFaceIDFailsTheCommandSayingSo() async {
        let passkey = Passkey()
        passkey.failure = ASAuthorizationError(.canceled)
        await #expect(throws: HerderError.Local(detail: "\(Biometry.name) was cancelled")) {
            try await FaceID { passkey }.assert(hostId: "h", challenge: challenge)
        }
    }
}
