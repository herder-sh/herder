import Herder
@testable import HerderKit
import Testing

struct PairScannerTests {
    let own = "herder://own?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=K7Q-4PD"
    var login: String { own.replacingOccurrences(of: "herder://own", with: "herder://login") }

    @Test func findsTheLinkAlone() {
        #expect(pairLink(in: own)?.approval?.kind == .machine)
        #expect(pairLink(in: "  \(login)\n")?.approval?.kind == .login)
    }

    @Test func findsTheLinkInHerderClaimOutput() {
        let output = """
            Scan the code with the herder app, or open this link:

            \(own)

            Check that the app shows K7Q · 4PD.
            """
        #expect(pairLink(in: output)?.approval?.code == "K7Q-4PD")
        #expect(pairLink(in: "open '\(own)'")?.approval?.code == "K7Q-4PD")
    }

    @Test func findsAnotherDevicesFleetLink() throws {
        let link = fleetLinkToString(link: FleetLink(person: FleetPerson(name: "alice", passkeys: ["Y3JlZC0x"]), machines: [
            FleetMachine(hosts: ["10.0.4.17:7447", "devbox.tail0.ts.net:7447"], fingerprint: String(repeating: "ab", count: 32)),
            FleetMachine(hosts: ["10.0.4.18:7447"], fingerprint: String(repeating: "cd", count: 32)),
        ]))
        let output = "Scan this code with herder on your other device, or paste:\n\n\(link)\n"
        guard case .fleet(let fleet)? = pairLink(in: output) else { Issue.record("not a fleet link"); return }
        #expect(fleet.machines.map(\.hosts) == [["10.0.4.17:7447", "devbox.tail0.ts.net:7447"], ["10.0.4.18:7447"]])
        #expect(fleet.machines.map(\.fingerprint) == [String(repeating: "ab", count: 32), String(repeating: "cd", count: 32)])
        #expect(fleet.person == FleetPerson(name: "alice", passkeys: ["Y3JlZC0x"]))
        #expect(pairLink(in: link)?.sheet == .join(fleet))
        #expect(pairLink(in: own)?.sheet == .approve(try #require(pairLink(in: own)?.approval)))
    }

    @Test func rejectsAFleetLinkWithNoMachine() {
        #expect(pairLink(in: "herder://fleet") == nil)
        #expect(pairLink(in: "herder://fleet?host=10.0.4.17%3A7447") == nil)
    }

    @Test func rejectsWhatIsNotAnOwnOrLoginLink() {
        #expect(pairLink(in: "") == nil)
        #expect(pairLink(in: "https://herder.sh") == nil)
        #expect(pairLink(in: "herder://own?host=127.0.0.1%3A7420") == nil)
        let pair = "herder://pair?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=ABCD2345"
        #expect(pairLink(in: pair) == nil)
    }
}

extension PairLink {
    /// The request to approve, for an own or login link.
    var approval: HerderKit.ApprovalRequest? {
        if case .approve(let request) = self { request } else { nil }
    }
}
