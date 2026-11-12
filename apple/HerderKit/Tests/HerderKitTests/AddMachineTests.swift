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
        let request = try #require(pairLink(in: output)?.approval)
        #expect(request.kind == .machine)
        #expect(request.name == "draugelis" && request.code == "K7Q-4PD" && request.fingerprint == fingerprint)
        #expect(request.hosts == ["127.0.0.1:7448"])
    }

    @Test func takesALoginLinkToApprove() throws {
        let request = try #require(pairLink(in: own.replacingOccurrences(of: "herder://own", with: "herder://login"))?.approval)
        #expect(request.kind == .login)
    }

    @Test func refusesPairingLinksAndCodes() {
        #expect(pairLink(in: "herder://pair?host=127.0.0.1%3A7448&fp=\(fingerprint)&code=ABCDE-FGHJK") == nil)
        #expect(pairLink(in: "address 127.0.0.1:7448\nfingerprint \(fingerprint)\ncode ABCDE-FGHJK") == nil)
        #expect(pairLink(in: "ABCDE-FGHJK") == nil)
        #expect(pairLink(in: "") == nil)
    }

    @Test func eachTabRunsWhatPrintsTheOwnLink() {
        #expect(MachineSource.allCases == [.install, .kubernetes, .hasHerder])
        #expect(MachineSource.allCases.map(\.title) == ["Linux or Mac", "Kubernetes", "Has herder"])
        #expect(MachineSource.install.command == "curl -fsSL https://herder.sh/install | sh\nherder service install")
        #expect(MachineSource.kubernetes.command.hasSuffix("kubectl exec -it -n herder herder-vault-0 -- /herder claim"))
        #expect(MachineSource.hasHerder.command == "herder claim")
        #expect(MachineSource.claimCommand == "herder claim")
    }

    @Test func progressGoesInstallApproveDone() throws {
        let request = try #require(pairLink(in: own)?.approval)
        #expect(MachineSource.steps == ["Install", "Approve", "Done"])
        #expect(AddMachineStep.install.index == 0)
        #expect(AddMachineStep.approve(.machine(request)).index == 1)
        #expect(AddMachineStep.done(.machine(request), "h1").index == 3, "every step done")
        #expect(AddMachineTarget.machine(request).name == "draugelis")
    }

    @Test func theApproverSaysWhichPasskeyAndWhereItIsKept() {
        let apple = PasskeyKey(credentialId: "AQ", publicKey: "pk", aaguid: "FBFC3007-154E-4ECC-8C0B-6E020557D7BD")
        let onePassword = PasskeyKey(credentialId: "Ag", publicKey: "pk", aaguid: "bada5566-a7aa-401f-bd96-45619a55120d")
        let unknown = PasskeyKey(credentialId: "Aw", publicKey: "pk")
        func person(_ keys: [PasskeyKey]) -> PersonInfo {
            PersonInfo(personId: "p-me", name: "Tomas", role: .owner, passkeys: keys, devices: [])
        }
        #expect(Approver.of(person([apple])) == Approver(name: "Tomas", detail: "Your passkey in Apple Passwords"))
        #expect(Approver.of(person([apple, onePassword, apple])).detail == "Your passkey in Apple Passwords or 1Password")
        #expect(Approver.of(person([unknown])).detail == "Your herder passkey", "a passkey that names no manager")
        #expect(Approver.of(nil) == .unknown)
        #expect(PasskeyHome.name(aaguid: nil) == nil)
    }

    @Test func doneSaysWhichDevicesCanUseTheNewMachine() {
        #expect(AddMachineView.coverage(Directory.Device(id: "mac", kind: .mac, current: true)) == "Signed in")
        #expect(AddMachineView.coverage(Directory.Device(id: "phone", kind: .phone)) == "Not on it yet: Add a Device brings it over")
        #expect(AddMachineView.coverage(Directory.Device(id: "tui", kind: .terminal, host: "trash-can-01")) == "Not needed")
    }
}
