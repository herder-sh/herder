import Herder
@testable import HerderKit
import Testing

struct AddMachineTests {
    let fingerprint = String(repeating: "ab", count: 32)
    var link: String { "herder://pair?host=127.0.0.1%3A7448&fp=\(fingerprint)&code=ABCDE-FGHJK" }
    var vault: UnpairedVault {
        UnpairedVault(address: "127.0.0.1:7448", fingerprint: fingerprint, hosts: ["draugelis"], machine: "draugelis")
    }

    @Test func saysTheRoleAndMachineTheLinkOfHerderPairNames() throws {
        let owner = try #require(PairInput("\(link)&role=owner&name=draugelis"))
        #expect(owner.role == .owner && owner.name == "draugelis" && owner.machines.count == 1)
        #expect(owner.grant(vault: nil).title == "You’ll join draugelis as owner")

        let member = try #require(PairInput("\(link)&role=member&name=draugelis"))
        #expect(member.grant(vault: nil) == ("You’ll join draugelis as member", "Drives sessions; no terminals or secrets."))
    }

    @Test func aLinkWithoutARoleJoinsWithTheSharersRole() throws {
        let shared = try #require(PairInput(link))
        #expect(shared.role == nil && shared.name == nil)
        #expect(shared.grant(vault: nil).title == "You’ll join this machine as yourself")
        // A chosen vault names it.
        #expect(shared.grant(vault: vault).title == "You’ll join draugelis’s vault as yourself")
    }

    @Test func takesTheLinesHerderPairPrintsWithoutTheLink() throws {
        let output = """
              address      127.0.0.1:7448
              address      10.0.0.9:7448
              fingerprint  \(fingerprint)
              code         ABCDE-FGHJK
            """
        let input = try #require(PairInput(output))
        #expect(input.machines == [PairingUri(hosts: ["127.0.0.1:7448", "10.0.0.9:7448"], fingerprint: fingerprint, code: "ABCDE-FGHJK")])
        // Without a fingerprint to pin, a code is not enough.
        #expect(PairInput("address 127.0.0.1:7448\ncode ABCDE-FGHJK") == nil)
    }

    @Test func takesTheBareCodeForAChosenVault() throws {
        #expect(PairInput("ABCDE-FGHJK") == nil)
        let input = try #require(PairInput(" abcdefghjk\n", vault: vault))
        #expect(input.machines == [PairingUri(hosts: ["127.0.0.1:7448"], fingerprint: fingerprint, code: "abcdefghjk")])
        #expect(!PairInput.blocked(input, vault: vault))
        // A bare code says no role: the sheet says how the command chose it.
        #expect(input.role == nil && !input.isLink)
        #expect(input.grant(vault: vault).title == "You’ll join draugelis’s vault with the role it printed")
        #expect(PairInput("not a code", vault: vault) == nil)
    }

    @Test func blocksALinkOfAnotherMachineThanTheChosenVault() throws {
        let other = try #require(PairInput(link.replacingOccurrences(of: fingerprint, with: String(repeating: "cd", count: 32))))
        #expect(PairInput.blocked(other, vault: vault))
        #expect(!PairInput.blocked(other, vault: nil))
        #expect(PairInput.blocked(nil, vault: nil))
    }
}
