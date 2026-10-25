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
        #expect(JobProgress(status(.attached, terminals: [prompted])).group == .needsYou)
    }

    @Test func theExitCodeTellsSignedInFromFailed() {
        #expect(status(.exited(0)) == .signedIn)
        #expect(status(.exited(1)) == .failed(1))
        #expect(status(.exited(1)).label == "Failed (exit 1)")
        #expect(status(.exited(nil)) == .failed(nil))
        #expect(JobProgress(status(.exited(0))).finished && JobProgress(status(.exited(1))).failed)
    }

    @Test func refusalsNameTheirCause() {
        #expect(status(.failed("aws is not installed"), refusal: .unsupported) == .notInstalled)
        #expect(status(.failed("no such profile"), refusal: .badRequest) == .noSuchProfile)
        #expect(status(.failed("timed out")) == .refused("timed out"))
    }

    @Test func aMachineThatCannotReadTheRequestNeedsANewerHerder() {
        let old = HerderError.Rejected(info: ErrorInfo(
            code: .badRequest, message: "invalid message: unknown variant `list_aws_profiles`"))
        #expect(AwsListProblem(error: old) == .outdated)
        #expect(AwsListProblem(error: old).label == "needs herder 0.8 or newer — run herder update")
        let forbidden = HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "owners only"))
        #expect(AwsListProblem(error: forbidden) == .failed("owners only"))
        #expect(AwsListProblem(error: HerderError.Closed) == .failed("herder stopped."))
        #expect(AwsSignInStatus.unlisted(.outdated).label == "Needs herder 0.8 or newer — run herder update")
        #expect(AwsSignInStatus.unlisted(.noProfiles).label == "No ~/.aws/config profiles")
    }

    @Test func nothingToPickSaysWhyWhenEveryMachineFailedAlike() {
        #expect(AwsListProblem.nothingToPick([.outdated, .outdated]).contains("herder update"))
        #expect(AwsListProblem.nothingToPick([.offline]) == "None of the machines you own is connected.")
        #expect(AwsListProblem.nothingToPick([.noProfiles]).contains("~/.aws/config"))
        #expect(AwsListProblem.nothingToPick([.outdated, .offline])
            == "No connected machine you own listed an AWS profile.")
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

    @Test func machinesAreSelectedWhenConnectedWithTheProfileAndNotLeftOut() {
        let machines = [
            machine("home", name: "home", sessions: []),
            machine("shared", name: "shared", sessions: []),
            machine("away", name: "away", sessions: [], connection: .disconnected(error: "gone")),
            machine("bare", name: "bare", sessions: []),
            machine("old", name: "old", sessions: []),
            machine("theirs", name: "theirs", sessions: [], role: .member),
        ]
        let prod = [AwsProfile(name: "trip1-prod", sso: true)]
        let profiles: [HostId: [AwsProfile]] = ["home": prod, "shared": prod, "bare": [AwsProfile(name: "dev", sso: true)],
                                                "theirs": prod]
        let problems: [HostId: AwsListProblem] = ["away": .offline, "old": .outdated]
        let selected = { (leftOut: Set<HostId>) in
            AwsSignIn.selected(machines, profile: "trip1-prod", profiles: profiles, problems: problems, leftOut: leftOut)
        }
        #expect(selected([]) == ["home", "shared"])
        #expect(selected(["shared"]) == ["home"])
        #expect(selected(["home", "shared"]).isEmpty)
        let blocker = { (hostId: HostId) in
            AwsSignIn.blocker(machines.first { $0.hostId == hostId }!, profile: "trip1-prod", profiles: profiles,
                              problems: problems)
        }
        #expect(blocker("home") == nil)
        #expect(blocker("away") == .offline)
        #expect(blocker("bare") == .noSuchProfile)
        #expect(blocker("old") == .unlisted(.outdated))
    }
}
