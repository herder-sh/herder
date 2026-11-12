import AuthenticationServices
import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor struct WelcomeTests {
    static let fingerprint = String(repeating: "ab", count: 32)
    static let own = "herder://own?host=192.168.1.20%3A7447&fp=\(fingerprint)&code=ROS-734&name=trash-can-01"

    @Test func theNewPathGoesNameMachineApproveAndBackAgain() {
        let flow = WelcomeFlow(name: "")
        #expect(flow.step == .choice && flow.progress == nil)
        flow.begin()
        #expect(flow.step == .name && flow.progress == 0)
        flow.confirmName()
        #expect(flow.step == .name, "a blank name goes no further")
        flow.name = "  Tomas "
        flow.confirmName()
        #expect(flow.step == .machine && flow.progress == 1)
        #expect(flow.person == "Tomas")
        flow.take("Scan the code, or open this link:\n\n\(Self.own)\n")
        #expect(flow.step == .approve && flow.progress == 2)
        #expect(flow.request?.name == "trash-can-01" && flow.request?.code == "ROS-734")
        flow.back()
        #expect(flow.step == .machine)
        flow.back()
        #expect(flow.step == .name && flow.person == "Tomas")
    }

    @Test func onlyANewMachinesLinkGoesOnToApprove() throws {
        let flow = WelcomeFlow(name: "Tomas")
        flow.step = .machine
        #expect(flow.take("") == nil)
        #expect(flow.problem == "Copy the herder://own link the machine prints, then paste it.")
        #expect(flow.take("hello") == nil)
        #expect(flow.problem == "That is not the herder://own link the machine prints.")
        #expect(flow.take(Self.own.replacingOccurrences(of: "herder://own", with: "herder://login")) == nil)
        #expect(flow.problem?.hasPrefix("That code signs a terminal in.") == true)
        #expect(flow.step == .machine)

        // Another device's code: you already use herder, so it signs in to those instead.
        let fleet = fleetLinkToString(link: FleetLink(machines: [
            FleetMachine(hosts: ["100.64.0.1:7447"], fingerprint: Self.fingerprint, name: "devbox"),
        ]))
        let shared = try #require(flow.take(fleet))
        #expect(shared.machines.map(\.name) == ["devbox"])
        #expect(flow.step == .machine && flow.problem == nil)
    }

    @Test func approvingMakesAPasskeyUnderTheNameGivenThenIsDone() async {
        let flow = WelcomeFlow(name: "Tomas ")
        flow.step = .machine
        flow.take(Self.own)
        var asked: [String] = []
        await flow.approve { request, name in
            #expect(request.fingerprint == Self.fingerprint)
            asked.append(name)
            return "trash-can-host"
        }
        #expect(asked == ["Tomas"])
        #expect(flow.step == .done && flow.progress == 3)
        #expect(flow.machine == "trash-can-host")
        #expect(!flow.approving)
    }

    @Test func closingThePasskeyPromptStaysToTryAgainButAFailureSaysWhy() async {
        let flow = WelcomeFlow(name: "Tomas")
        flow.step = .machine
        flow.take(Self.own)
        await flow.approve { _, _ in throw ASAuthorizationError(.canceled) }
        #expect(flow.step == .approve && flow.problem == nil)
        await flow.approve { _, _ in throw HerderError.Local(detail: "the code has expired") }
        #expect(flow.step == .approve)
        #expect(flow.problem?.contains("the code has expired") == true)
    }

    @Test func aMachineAddedAnotherWayEndsWelcomeOnlyOffTheNewPathsLastSteps() {
        let flow = WelcomeFlow(name: "Tomas")
        #expect(flow.yieldsToAMachine)
        flow.step = .name
        #expect(flow.yieldsToAMachine)
        flow.step = .approve
        #expect(!flow.yieldsToAMachine)
        flow.step = .done
        #expect(!flow.yieldsToAMachine)
    }

    @Test func eachWayToInstallSaysWhatToRun() {
        #expect(WelcomeFlow.Install.linuxOrMac.command == "curl -fsSL https://herder.sh/install | sh\nherder service install")
        #expect(WelcomeFlow.Install.hasHerder.command == "herder claim")
        #expect(WelcomeFlow.Install.container.command.hasSuffix("herder claim"))
    }
}
